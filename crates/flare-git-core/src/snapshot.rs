//! Pre-destructive snapshotting: before a destructive git command runs
//! (`reset --hard`, `clean -f*`, force checkout/switch — see
//! `classify::is_destructive`), the shim snapshots the current working
//! tree so it can be recovered. Snapshots are plain git commit objects
//! under a private ref namespace (`refs/agentflare/snapshots/<sha>`) — no
//! separate blob store or metadata DB; git's own object store already
//! does exactly this job, and a ref under a private namespace keeps the
//! commit reachable (gc-safe) without touching the working tree or the
//! real index while creating it.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::shell::run_in;

const SNAPSHOT_REF_PREFIX: &str = "refs/agentflare/snapshots/";

/// Process-local counter mixed into temporary index filenames so two
/// snapshot calls racing inside the same process (distinct threads, same
/// PID) never share a `GIT_INDEX_FILE` -- `std::process::id()` alone only
/// guarantees uniqueness across processes.
static SNAPSHOT_CALL_ID: AtomicU64 = AtomicU64::new(0);

fn unique_index_suffix() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SNAPSHOT_CALL_ID.fetch_add(1, Ordering::Relaxed)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotId(pub String);

#[derive(Debug, Clone)]
pub struct SnapshotMeta {
    pub id: SnapshotId,
    pub committer_date: String,
    pub reason: String,
}

/// `git <args>` with a temporary `GIT_INDEX_FILE`, so staging for a
/// snapshot never touches the caller's real index.
///
/// `cwd` is the directory git runs in. It is the repo root for a plain
/// repo-root snapshot, but `snapshot_worktree_before` points it at the
/// worktree being captured instead — `git add -A` resolves pathspecs
/// against the current directory, so running it from the repo root with
/// only `--work-tree` set makes git refuse the paths as outside the
/// repository.
fn run_git_with_index(cwd: &Path, index_file: &Path, args: &[&str]) -> Result<String, String> {
    // A snapshot must capture exactly what's on disk right now -- `-c
    // core.autocrlf=false` stops git silently converting line endings
    // while staging, regardless of the caller's ambient/global git config
    // (autocrlf=true is the common default on Windows).
    let mut cmd = Command::new(crate::shell::git_binary());
    cmd.args(["-c", "core.autocrlf=false"])
        .args(args)
        .current_dir(cwd)
        .env("GIT_INDEX_FILE", index_file);
    crate::shell::apply_filtered_path(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("git not available: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Snapshots the current working tree (tracked + untracked, respecting
/// `.gitignore`) into a commit object under a private ref. Staging happens
/// in a temporary index file, removed afterward regardless of outcome —
/// the real index and working tree are never touched.
pub fn snapshot_before(repo_root: &Path, reason: &str) -> Result<SnapshotId, String> {
    let tmp_index = repo_root.join(".git").join(format!(
        "agentflare-snapshot-index-{}",
        unique_index_suffix()
    ));
    let result = (|| {
        run_git_with_index(repo_root, &tmp_index, &["add", "-A"])?;
        let tree = run_git_with_index(repo_root, &tmp_index, &["write-tree"])?;
        let parent = run_in(repo_root, &["rev-parse", "HEAD"]).ok();
        let mut commit_args = vec![
            "commit-tree".to_string(),
            tree,
            "-m".to_string(),
            reason.to_string(),
        ];
        if let Some(p) = &parent {
            commit_args.push("-p".to_string());
            commit_args.push(p.clone());
        }
        let commit_args_ref: Vec<&str> = commit_args.iter().map(String::as_str).collect();
        let sha = run_in(repo_root, &commit_args_ref)?;
        let refname = format!("{SNAPSHOT_REF_PREFIX}{sha}");
        run_in(repo_root, &["update-ref", &refname, &sha])?;
        Ok(SnapshotId(sha))
    })();
    let _ = std::fs::remove_file(&tmp_index);
    result
}

/// Snapshots a *linked worktree's* contents into the main repo's object
/// store, returning a recoverable commit under the private snapshot ref.
///
/// [`snapshot_before`] cannot do this job: it stages from `repo_root`, and
/// `ensure_worktrees_ignored` puts `.worktrees/` in `.git/info/exclude`, so
/// `git add -A` there captures exactly nothing of the worktree about to be
/// deleted. Every "snapshot first, then remove" call site was therefore
/// writing an empty safety net and destroying uncommitted work anyway.
///
/// Works even when the worktree's own `.git` pointer is broken (the
/// orphan case): the object store is addressed explicitly via `--git-dir`
/// on the main repo rather than discovered from the worktree. The
/// worktree's own `.gitignore` still applies, so build artifacts
/// (`target/`) and the generated `.cargo/` stay out of the snapshot.
///
/// The commit is deliberately parentless — the worktree's branch tip is
/// not necessarily its content's ancestor (a broken-gitdir worktree may
/// not have a resolvable branch at all), and recovery only needs the tree.
pub fn snapshot_worktree_before(
    repo_root: &Path,
    worktree_path: &Path,
    reason: &str,
) -> Result<SnapshotId, String> {
    if !worktree_path.exists() {
        return Err(format!(
            "worktree path does not exist: {}",
            worktree_path.display()
        ));
    }
    let common_dir = resolve_common_dir(repo_root);
    let tmp_index = common_dir.join(format!(
        "agentflare-worktree-snapshot-index-{}",
        unique_index_suffix()
    ));
    let git_dir = common_dir.to_string_lossy().to_string();
    let result = (|| {
        let stage_args = ["--git-dir", &git_dir, "add", "-A", "."];
        run_git_with_index(worktree_path, &tmp_index, &stage_args)?;
        let tree = run_git_with_index(
            worktree_path,
            &tmp_index,
            &["--git-dir", &git_dir, "write-tree"],
        )?;
        let sha = run_in(repo_root, &["commit-tree", &tree, "-m", reason])?;
        let refname = format!("{SNAPSHOT_REF_PREFIX}{sha}");
        run_in(repo_root, &["update-ref", &refname, &sha])?;
        Ok(SnapshotId(sha))
    })();
    let _ = std::fs::remove_file(&tmp_index);
    result
}

/// Absolute path to the repo's common git dir — the shared `.git` of the
/// main checkout, which is where linked worktrees' objects and admin
/// entries actually live. Falls back to `<repo_root>/.git` when git can't
/// answer, matching this module's best-effort style.
fn resolve_common_dir(repo_root: &Path) -> std::path::PathBuf {
    match run_in(
        repo_root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) {
        Ok(p) if !p.trim().is_empty() => std::path::PathBuf::from(p.trim()),
        _ => repo_root.join(".git"),
    }
}

/// Checks out `commit_ish`'s tracked files into the current working tree and
/// index, without moving HEAD. Only restores paths that existed at
/// `commit_ish` -- files created since are untouched and survive.
///
/// "-c core.autocrlf=false" for the same reason run_git_with_index sets it
/// on the capture side: a restore is a promise of exact recovery, and git
/// silently rewriting LF to CRLF on checkout (autocrlf=true, common on
/// Windows) breaks that promise regardless of what was actually snapshotted.
pub fn checkout_into_worktree(repo_root: &Path, commit_ish: &str) -> Result<(), String> {
    run_in(
        repo_root,
        &[
            "-c",
            "core.autocrlf=false",
            "checkout",
            commit_ish,
            "--",
            ".",
        ],
    )?;
    Ok(())
}

/// Restores paths from a snapshot into the current working tree and index.
/// Only restores paths that existed at snapshot time -- files created after
/// the snapshot are untouched and survive.
pub fn restore(repo_root: &Path, id: &SnapshotId) -> Result<(), String> {
    let refname = format!("{SNAPSHOT_REF_PREFIX}{}", id.0);
    checkout_into_worktree(repo_root, &refname)
}

/// Lists snapshots, newest first.
#[must_use]
pub fn list(repo_root: &Path) -> Vec<SnapshotMeta> {
    let Ok(out) = run_in(
        repo_root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname) %(committerdate:iso-strict) %(subject)",
            SNAPSHOT_REF_PREFIX,
        ],
    ) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            let refname = parts.next()?;
            let date = parts.next()?;
            let reason = parts.next().unwrap_or("").to_string();
            let id = refname.strip_prefix(SNAPSHOT_REF_PREFIX)?.to_string();
            Some(SnapshotMeta {
                id: SnapshotId(id),
                committer_date: date.to_string(),
                reason,
            })
        })
        .collect()
}

/// Deletes all but the `keep_last` most recent snapshots.
pub fn prune(repo_root: &Path, keep_last: usize) -> Result<(), String> {
    for meta in list(repo_root).into_iter().skip(keep_last) {
        let refname = format!("{SNAPSHOT_REF_PREFIX}{}", meta.id.0);
        run_in(repo_root, &["update-ref", "-d", &refname])?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::test_support::init_repo_with_branch;
    use tempfile::TempDir;

    #[test]
    fn snapshot_then_restore_recovers_state_and_preserves_newer_files() {
        let repo = init_repo_with_branch("master");
        std::fs::write(repo.path.join("tracked.txt"), "before\n").unwrap();
        run_in(&repo.path, &["add", "tracked.txt"]).unwrap();
        run_in(&repo.path, &["commit", "-m", "add tracked"]).unwrap();
        std::fs::write(repo.path.join("tracked.txt"), "modified\n").unwrap();
        std::fs::write(repo.path.join("untracked.txt"), "scratch\n").unwrap();

        let id = snapshot_before(&repo.path, "pre reset --hard").unwrap();

        // Simulate the destructive op the snapshot exists to protect against.
        run_in(&repo.path, &["checkout", "--", "tracked.txt"]).unwrap(); // discards "modified"
        std::fs::remove_file(repo.path.join("untracked.txt")).unwrap();
        // A file created AFTER the snapshot -- must survive restore.
        std::fs::write(repo.path.join("new_after_snapshot.txt"), "keep me\n").unwrap();

        restore(&repo.path, &id).unwrap();

        assert_eq!(
            std::fs::read_to_string(repo.path.join("tracked.txt")).unwrap(),
            "modified\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.path.join("untracked.txt")).unwrap(),
            "scratch\n"
        );
        assert!(
            repo.path.join("new_after_snapshot.txt").exists(),
            "file created after snapshot must survive restore"
        );
    }

    #[test]
    fn snapshot_then_restore_is_byte_exact_regardless_of_autocrlf() {
        // Regression: without an explicit "-c core.autocrlf=false" on both
        // the capture and restore sides, a repo/global config of
        // core.autocrlf=true (the common Windows default -- this is exactly
        // what tripped CI on windows-latest) makes git checkout silently
        // rewrite LF to CRLF on restore, breaking the "exact recovery"
        // promise this feature exists for. Sets core.autocrlf=true locally
        // on the test repo rather than relying on the host's ambient config,
        // so this reproduces the CI failure deterministically everywhere.
        let repo = init_repo_with_branch("master");
        run_in(&repo.path, &["config", "core.autocrlf", "true"]).unwrap();
        std::fs::write(repo.path.join("tracked.txt"), "before\n").unwrap();
        run_in(&repo.path, &["add", "tracked.txt"]).unwrap();
        run_in(&repo.path, &["commit", "-m", "add tracked"]).unwrap();
        std::fs::write(repo.path.join("tracked.txt"), "modified\n").unwrap();

        let id = snapshot_before(&repo.path, "pre reset --hard").unwrap();
        run_in(&repo.path, &["checkout", "--", "tracked.txt"]).unwrap();
        restore(&repo.path, &id).unwrap();

        let bytes = std::fs::read(repo.path.join("tracked.txt")).unwrap();
        assert_eq!(
            bytes, b"modified\n",
            "restore must not let core.autocrlf touch recovered bytes"
        );
    }

    #[test]
    fn list_and_prune_keep_only_the_most_recent() {
        let repo = init_repo_with_branch("master");
        let id1 = snapshot_before(&repo.path, "first").unwrap();
        // Distinct committerdate second-resolution ordering.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(repo.path.join("f.txt"), "x").unwrap();
        let id2 = snapshot_before(&repo.path, "second").unwrap();

        let listed = list(&repo.path);
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[0].id, id2, "newest first");
        assert_eq!(listed[1].id, id1);

        prune(&repo.path, 1).unwrap();
        let after = list(&repo.path);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, id2);
    }

    #[test]
    fn concurrent_worktree_snapshots_do_not_share_an_index() {
        // Regression: snapshot_worktree_before used to key its temporary
        // GIT_INDEX_FILE on std::process::id() alone. Two threads in the
        // same process snapshotting two different worktrees at once would
        // stomp on each other's staging area, so a snapshot could end up
        // capturing the wrong worktree's contents (or a mix of both).
        let repo = init_repo_with_branch("master");
        let wt_a = repo.path.join("wt-a");
        let wt_b = repo.path.join("wt-b");
        run_in(
            &repo.path,
            &[
                "worktree",
                "add",
                "-b",
                "wt-a",
                wt_a.to_str().unwrap(),
                "master",
            ],
        )
        .unwrap();
        run_in(
            &repo.path,
            &[
                "worktree",
                "add",
                "-b",
                "wt-b",
                wt_b.to_str().unwrap(),
                "master",
            ],
        )
        .unwrap();
        std::fs::write(wt_a.join("sentinel-a.txt"), "only in a\n").unwrap();
        std::fs::write(wt_b.join("sentinel-b.txt"), "only in b\n").unwrap();

        let repo_root_a = repo.path.clone();
        let wt_a_clone = wt_a.clone();
        let handle_a = std::thread::spawn(move || {
            snapshot_worktree_before(&repo_root_a, &wt_a_clone, "concurrent snapshot a").unwrap()
        });
        let repo_root_b = repo.path.clone();
        let wt_b_clone = wt_b.clone();
        let handle_b = std::thread::spawn(move || {
            snapshot_worktree_before(&repo_root_b, &wt_b_clone, "concurrent snapshot b").unwrap()
        });
        let id_a = handle_a.join().unwrap();
        let id_b = handle_b.join().unwrap();

        let tree_a = run_in(&repo.path, &["ls-tree", "-r", "--name-only", &id_a.0]).unwrap();
        let tree_b = run_in(&repo.path, &["ls-tree", "-r", "--name-only", &id_b.0]).unwrap();
        assert!(
            tree_a.contains("sentinel-a.txt") && !tree_a.contains("sentinel-b.txt"),
            "snapshot a must capture only worktree a's contents: {tree_a}"
        );
        assert!(
            tree_b.contains("sentinel-b.txt") && !tree_b.contains("sentinel-a.txt"),
            "snapshot b must capture only worktree b's contents: {tree_b}"
        );
    }

    #[test]
    fn snapshot_before_any_commit_exists_still_works() {
        // No parent commit to attach to -- must not error out on a
        // brand-new, still-empty-history repo (no initial commit).
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        run_in(&path, &["init", "-b", "master"]).unwrap();
        run_in(&path, &["config", "user.email", "test@test.com"]).unwrap();
        run_in(&path, &["config", "user.name", "Test"]).unwrap();
        std::fs::write(path.join("f.txt"), "x").unwrap();
        let id = snapshot_before(&path, "no parent yet");
        assert!(id.is_ok(), "{id:?}");
    }
}
