//! Self-heal helpers for per-item worktrees: clearing the wedged state a
//! killed git leaves behind, adopting an existing checkout in place,
//! `git worktree lock` bookkeeping, and the lease-guarded push path.
//! Child module of `worktree` (split out for size); items are re-imported
//! there via `use heal::*`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{REBASE_TIMEOUT_SECS, run_output_timeout};
use crate::shell::{run_in as run_git_in, run_in_ok as run_git_in_ok};

/// [`run_output_timeout`] against git, shaped like `shell::run_in`:
/// `Ok(stdout)` trimmed on success, `Err(stderr)` (or the spawn/timeout
/// message) otherwise.
pub(super) fn run_git_timeout(
    cwd: &Path,
    args: &[&str],
    timeout_secs: u64,
) -> Result<String, String> {
    let out = run_output_timeout(crate::shell::git_binary(), args, cwd, timeout_secs)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// How old a leftover `index.lock` must be before it's presumed abandoned by
/// a killed git process rather than held by a live one. Generous on purpose:
/// no single index-mutating git operation this crate runs comes close.
pub(super) const STALE_INDEX_LOCK_AGE: Duration = Duration::from_secs(10 * 60);

/// Resolves a path inside `worktree_path`'s own git dir (`index.lock`,
/// `rebase-merge`, ...) -- per-worktree for a linked worktree.
pub(super) fn worktree_git_path(worktree_path: &Path, name: &str) -> Option<PathBuf> {
    let raw = run_git_in(worktree_path, &["rev-parse", "--git-path", name]).ok()?;
    let p = PathBuf::from(raw);
    Some(if p.is_absolute() {
        p
    } else {
        worktree_path.join(p)
    })
}

/// `true` when `path` is the top level of its own git checkout. A task
/// worktree whose `.git` pointer is missing or broken would otherwise let
/// every git command run "in" it silently operate on the enclosing main
/// repository instead (`.worktrees/` lives inside it).
pub(super) fn is_own_checkout(path: &Path) -> bool {
    let Ok(top) = run_git_in(path, &["rev-parse", "--show-toplevel"]) else {
        return false;
    };
    match (Path::new(&top).canonicalize(), path.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Clears the wedged state a killed or timed-out git leaves in a worktree,
/// which otherwise blocks every later commit/rebase/re-claim with nothing to
/// clear it automatically: an abandoned `index.lock`, and a half-finished
/// rebase (detached `HEAD`, `rebase-merge`/`rebase-apply` left behind).
///
/// `lock_is_ours` skips the age check -- for a caller that just killed the
/// git process holding the lock itself. Returns `Err` if a rebase is still
/// in progress afterward.
pub(super) fn heal_interrupted_git_state(
    worktree_path: &Path,
    lock_is_ours: bool,
) -> Result<(), String> {
    if !is_own_checkout(worktree_path) {
        return Ok(());
    }
    if let Some(lock) = worktree_git_path(worktree_path, "index.lock")
        && let Ok(meta) = std::fs::metadata(&lock)
    {
        let age = meta
            .modified()
            .ok()
            .and_then(|m| m.elapsed().ok())
            .unwrap_or_default();
        if (lock_is_ours || age >= STALE_INDEX_LOCK_AGE) && std::fs::remove_file(&lock).is_ok() {
            eprintln!(
                "worktree: removed abandoned {} ({}s old)",
                lock.display(),
                age.as_secs()
            );
        }
    }
    let rebasing = ["rebase-merge", "rebase-apply"]
        .iter()
        .filter_map(|n| worktree_git_path(worktree_path, n))
        .any(|p| p.exists());
    if !rebasing {
        return Ok(());
    }
    run_git_in(worktree_path, &["rebase", "--abort"])
        .map(|_| {
            eprintln!(
                "worktree: aborted interrupted rebase in {}",
                worktree_path.display()
            )
        })
        .map_err(|e| {
            format!(
                "worktree: {} is stuck mid-rebase and `git rebase --abort` failed: {e}",
                worktree_path.display()
            )
        })
}

/// When `worktree_path` is on a detached `HEAD` whose commits no branch or
/// remote-tracking ref contains, pins them under
/// `refs/agentflare/rescue/...` so removing or switching the checkout can't
/// make them unreachable (`worktree prune` drops the checkout's `HEAD`
/// reflog, the only other thing keeping them alive). Returns the rescue ref.
pub(super) fn rescue_detached_head(worktree_path: &Path, label: &str) -> Option<String> {
    let current = run_git_in(worktree_path, &["branch", "--show-current"]).ok()?;
    if !current.is_empty() {
        return None;
    }
    let head = run_git_in(worktree_path, &["rev-parse", "HEAD"]).ok()?;
    let reachable = run_git_in(
        worktree_path,
        &[
            "for-each-ref",
            "--count=1",
            "--contains",
            &head,
            "refs/heads",
            "refs/remotes",
        ],
    )
    .map(|out| !out.is_empty())
    .unwrap_or(true);
    if reachable {
        return None;
    }
    let short: String = head.chars().take(12).collect();
    let rescue = format!("refs/agentflare/rescue/{label}-{short}");
    run_git_in(worktree_path, &["update-ref", &rescue, &head]).ok()?;
    eprintln!(
        "worktree: detached HEAD {short} in {} held commits no branch contains -- saved as {rescue}",
        worktree_path.display()
    );
    Some(rescue)
}

/// Puts an existing, clean checkout at `worktree_path` onto `branch` in
/// place -- the recovery for a task worktree left on a detached `HEAD` (an
/// agent's `git checkout <sha>`, an interrupted rebase) or on some other
/// branch. Without it, `git worktree add` refuses the occupied path forever
/// and every redispatch of the item fails identically.
///
/// Never loses commits: detached work that `branch` doesn't already contain
/// is fast-forwarded onto `branch` when possible, otherwise pinned by
/// [`rescue_detached_head`] first.
pub(super) fn adopt_existing_checkout(
    worktree_path: &Path,
    branch: &str,
    label: &str,
) -> Result<(), String> {
    let detached = run_git_in(worktree_path, &["branch", "--show-current"])?.is_empty();
    let local = format!("refs/heads/{branch}");
    let remote = format!("refs/remotes/origin/{branch}");
    let existing = [&local, &remote]
        .into_iter()
        .find(|r| run_git_in_ok(worktree_path, &["rev-parse", "--verify", "--quiet", r]));
    let is_ancestor =
        |a: &str, b: &str| run_git_in_ok(worktree_path, &["merge-base", "--is-ancestor", a, b]);
    let args: Vec<&str> = match existing {
        // Nothing to preserve beyond what HEAD already has: branch from it.
        None => vec!["switch", "-c", branch],
        Some(r) if detached && !is_ancestor("HEAD", r) && is_ancestor(r, "HEAD") => {
            // Detached work strictly ahead of the branch: fast-forward.
            vec!["switch", "-C", branch]
        }
        Some(r) => {
            if detached && !is_ancestor("HEAD", r) {
                rescue_detached_head(worktree_path, label);
            }
            if r == &local {
                vec!["switch", branch]
            } else {
                vec!["switch", "-c", branch, "--track", &remote]
            }
        }
    };
    run_git_in(worktree_path, &args).map(|_| {
        eprintln!(
            "worktree: switched existing checkout {} onto {branch}",
            worktree_path.display()
        );
    })
}

/// Reason recorded by [`lock_item_worktree`]; registrations locked with it
/// are ours to clean up, any other lock is someone's deliberate pin.
pub(super) const AGENTFLARE_LOCK_REASON: &str = "agentflare: in use by work item";

/// `git worktree lock`s a claimed item's worktree so nothing outside this
/// process (a human's `git worktree prune`, `gc`'s own pruning, another
/// tool) drops its registration while an agent works in it. Idempotent.
pub(super) fn lock_item_worktree(repo_root: &Path, worktree_path: &Path) {
    let path = worktree_path.to_string_lossy().to_string();
    let _ = run_git_in(
        repo_root,
        &[
            "worktree",
            "lock",
            "--reason",
            AGENTFLARE_LOCK_REASON,
            &path,
        ],
    );
}

/// Whether a registration's `locked` file is absent or was written by
/// [`lock_item_worktree`] -- i.e. whether our own cleanup may remove it.
pub(super) fn lock_is_ours_or_absent(admin: &Path) -> bool {
    match std::fs::read_to_string(admin.join("locked")) {
        Ok(reason) => reason.trim() == AGENTFLARE_LOCK_REASON,
        Err(_) => true,
    }
}

/// `true` when `worktree_path`'s registration still carries the
/// "initializing" lock `git worktree add` holds while it populates a new
/// checkout, and is old enough that no add can still be in flight -- the
/// signature of an add killed partway (crash, timeout). Such a checkout can
/// be missing most of its files while already on the right branch; reusing
/// it would stage every unpopulated file as a deletion.
pub(super) fn is_half_created(worktree_path: &Path) -> bool {
    let Some(locked) = worktree_git_path(worktree_path, "locked") else {
        return false;
    };
    let Ok(reason) = std::fs::read_to_string(&locked) else {
        return false;
    };
    let old_enough = std::fs::metadata(&locked)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age >= Duration::from_secs(120));
    reason.contains("initializing") && old_enough
}

/// Resolves a gitdir pointer as git itself does: absolute as-is, relative
/// (`worktree.useRelativePaths`, git 2.48+) against the directory holding
/// the file it was read from.
pub(super) fn resolve_gitdir_pointer(base_dir: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw.trim());
    if p.is_absolute() {
        return p.to_path_buf();
    }
    // Lexically, not `canonicalize`: the stale-registration callers need
    // this for paths that no longer exist.
    let mut out = PathBuf::new();
    for c in base_dir.join(p).components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// `git push --force-with-lease --force-if-includes`: the lease alone checks
/// against `origin/<branch>`, which ANY fetch in this repo (every worktree
/// shares it) can advance past commits this branch never saw -- the push
/// would then silently drop them. `--force-if-includes` additionally
/// requires the remote tip to be in this branch's reflog. Falls back to the
/// lease alone on a git too old to know the flag (< 2.30).
pub(super) fn push_with_lease(repo_root: &Path, branch: &str) -> Result<(), String> {
    const PUSH_TIMEOUT_SECS: u64 = 120;
    let with_includes = [
        "push",
        "--force-with-lease",
        "--force-if-includes",
        "-u",
        "origin",
        branch,
    ];
    match run_git_timeout(repo_root, &with_includes, PUSH_TIMEOUT_SECS) {
        Err(e) if e.contains("force-if-includes") => run_git_timeout(
            repo_root,
            &["push", "--force-with-lease", "-u", "origin", branch],
            PUSH_TIMEOUT_SECS,
        )
        .map(|_| ()),
        other => other.map(|_| ()),
    }
}

pub(super) fn is_push_rejection(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("rejected") || e.contains("stale info") || e.contains("non-fast-forward")
}

/// Fetches `origin/<branch>` and rebases the worktree onto it, so commits
/// pushed there by someone else survive the next push. Patches this branch
/// already carries (its own earlier, pre-rebase pushes) are dropped by the
/// rebase's patch-id check. Aborts cleanly on conflict.
pub(super) fn integrate_remote_branch(worktree_path: &Path, branch: &str) -> Result<(), String> {
    let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    run_git_timeout(worktree_path, &["fetch", "origin", &refspec], 30)?;
    let remote = format!("refs/remotes/origin/{branch}");
    match run_git_timeout(worktree_path, &["rebase", &remote], REBASE_TIMEOUT_SECS) {
        Ok(_) => Ok(()),
        Err(e) => {
            let _ = heal_interrupted_git_state(worktree_path, true);
            Err(e)
        }
    }
}
