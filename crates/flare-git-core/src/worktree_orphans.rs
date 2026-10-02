//! Orphaned-worktree audit/GC and worktree-directory removal for
//! `worktree`. Child module of `worktree` (split out for size); public
//! items are re-exported from there, so `flare_git_core::worktree::X`
//! paths are unchanged.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{remove_stale_registration_for_path, resolve_gitdir_pointer, same_location};
use crate::shell::run_in as run_git_in;

/// What a worktree directory's `.git` says about it, read straight off disk
/// -- no git command involved, so a failing or misdirected git can't change
/// the answer.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum GitPointer {
    /// No `.git` at all (an unreadable one is `Intact`: unknown is not broken).
    Missing,
    /// A pointer file naming an admin directory that no longer exists.
    Dangling,
    /// A pointer file whose admin directory exists (or a real `.git` dir).
    Intact,
}

pub(super) fn git_pointer(path: &Path) -> GitPointer {
    let dot_git = path.join(".git");
    if dot_git.is_dir() {
        return GitPointer::Intact;
    }
    let content = match std::fs::read_to_string(&dot_git) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return GitPointer::Missing,
        // Locked by an antivirus scan or an editor, say: not evidence the
        // worktree is broken, and callers clear a directory only on that.
        Err(_) => return GitPointer::Intact,
    };
    let gitdir = content.trim().trim_start_matches("gitdir: ");
    if resolve_gitdir_pointer(path, gitdir).exists() {
        GitPointer::Intact
    } else {
        GitPointer::Dangling
    }
}

/// Whether `worktree_path` is structurally not a worktree of `repo_root`:
/// its `.git` pointer is missing or dangling, or git does not list it as a
/// registered worktree. This is the only ground on which `create_worktree`
/// may clear an existing directory -- "a git command failed inside it" is
/// not (item #689: a leaked `GIT_WORK_TREE` made `rev-parse` answer for the
/// main repo and a live worktree was garbage-collected). Fails closed: an
/// unreadable registration list is not evidence of anything.
///
/// `audit_orphans` shares the pointer half (`git_pointer`) but not this
/// function: a directory with no `.git` of its own is skipped there (never
/// classified or gc'd from the enclosing repo's answers), whereas for
/// `create_worktree` that same state is the one it may clear and recreate.
pub(super) fn is_structurally_broken(repo_root: &Path, worktree_path: &Path) -> bool {
    if worktree_path.join(".git").is_dir() {
        return false;
    }
    match git_pointer(worktree_path) {
        GitPointer::Missing | GitPointer::Dangling => true,
        GitPointer::Intact => run_git_in(repo_root, &["worktree", "list", "--porcelain"])
            .is_ok_and(|list| {
                !list
                    .lines()
                    .filter_map(|l| l.strip_prefix("worktree "))
                    .any(|listed| same_location(Path::new(listed), worktree_path))
            }),
    }
}

/// Information about an orphaned worktree detected during audit.
pub struct OrphanWorktree {
    pub name: String,
    pub path: PathBuf,
    pub sequence_id: Option<i64>,
    pub size_bytes: u64,
    pub has_broken_gitdir: bool,
    /// The worktree's gitdir is intact but it sits on the repo's default
    /// branch (a task worktree stranded there) -- the gh merge-collision
    /// case. Mutually exclusive-ish with `has_broken_gitdir`: at least one
    /// of the two is always true for an orphan.
    pub on_default_branch: bool,
}

/// Scan `.worktrees/task/*` for orphaned worktree directories.
///
/// A worktree is considered orphaned when its `.git` gitdir pointer is
/// broken (the file exists but points to a path that no longer exists).
/// This typically means `git worktree remove` was interrupted or the
/// repository's `.git/worktrees/<name>` metadata was manually cleaned
/// up without removing the worktree's working directory.
///
/// When `claimed_item_ids` is provided, any directory whose name matches
/// a live item's sequence_id is excluded from the orphan list — that
/// worktree may simply need a metadata fix rather than removal.
pub fn audit_orphans(
    repo_root: &Path,
    claimed_item_ids: Option<&std::collections::HashSet<String>>,
) -> Vec<OrphanWorktree> {
    let task_dir = repo_root.join(".worktrees").join("task");
    if !task_dir.exists() {
        return Vec::new();
    }
    let default_branch = crate::branch::resolve_default_branch(repo_root);
    let mut orphans = Vec::new();
    // filter_map skips unreadable entries individually rather than failing
    // the whole scan on the first error -- a single permission-denied or
    // transient FS error on one subdirectory must not hide real orphans
    // sitting alongside it.
    let entries: Vec<_> = walkdir::WalkDir::new(&task_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .collect();
    for entry in entries.iter().skip(1) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let has_broken_gitdir = git_pointer(path) == GitPointer::Dangling;
        // Exclude directories whose name matches a live claimed item
        if let Some(claimed) = claimed_item_ids
            && claimed.contains(&dir_name)
        {
            continue;
        }
        // Only consider as orphan when the gitdir pointer is broken, OR the
        // worktree is sitting on the repo's default branch. The second case
        // is the stranded-worktree root cause behind gh pr merge --delete-
        // branch / post-merge local-sync failures (item #441, vents #351/
        // #394/#423): a task worktree should only ever be on its own
        // task/<N> branch, so one that has been switched to the default
        // branch is abandoned junk -- it holds the default branch checked
        // out, which blocks both deleting it and gh's merge flow.
        let mut on_default_branch = false;
        if !has_broken_gitdir {
            // A dir with no working `.git` of its own would have the branch
            // and status checks below silently run against the enclosing
            // main repo -- never classify (and gc) it from that.
            if !super::is_own_checkout(path) {
                continue;
            }
            on_default_branch = crate::branch::current_branch(path)
                .map(|b| !b.is_empty() && b == default_branch)
                .unwrap_or(false);
            if !on_default_branch {
                continue;
            }
            // Stranded-but-dirty is still real work -- same guard
            // `cleanup_item_worktree` applies before its own `gc_orphans`
            // call; fail closed (preserve) on a status-check error too.
            let clean = matches!(run_git_in(path, &["status", "--porcelain"]), Ok(o) if o.trim().is_empty());
            if !clean {
                eprintln!(
                    "worktree: preserving {} -- uncommitted changes",
                    path.display()
                );
                continue;
            }
        }
        let sequence_id = dir_name.parse::<i64>().ok();
        let size_bytes = dir_size(path);
        orphans.push(OrphanWorktree {
            name: dir_name,
            path: path.to_path_buf(),
            sequence_id,
            size_bytes,
            has_broken_gitdir,
            on_default_branch,
        });
    }
    orphans
}

/// Delete orphaned worktrees: snapshot first, then remove with retry.
///
/// Takes a list of worktree names (as returned by `audit_orphans`) and
/// snapshots each one before removing it. Removal uses `remove_worktree_dir`,
/// which either takes the whole directory or leaves it untouched. Returns
/// the names actually deleted.
pub fn gc_orphans(repo_root: &Path, names: &[String]) -> Vec<String> {
    let mut deleted = Vec::new();
    for name in names {
        let worktree_path = repo_root.join(".worktrees").join("task").join(name);
        if !worktree_path.exists() {
            continue;
        }
        // Snapshot before deletion -- abort this orphan's removal if the
        // snapshot itself fails, since that snapshot is the only recovery
        // point a destructive delete has; deleting anyway would defeat the
        // whole point of snapshotting first.
        let reason = format!("gc orphan worktree {}", name);
        if let Err(e) =
            crate::snapshot::snapshot_worktree_before(repo_root, &worktree_path, &reason)
        {
            eprintln!(
                "worktree: skipping orphan '{}', snapshot failed: {}",
                name, e
            );
            continue;
        }
        let path = worktree_path.to_string_lossy().to_string();
        let _ = run_git_in(repo_root, &["worktree", "unlock", &path]);
        if remove_worktree_dir(&worktree_path, name) {
            // Scoped to this path, never a repo-wide `worktree prune`: prune
            // also drops registrations of other worktrees whose gitdir merely
            // looks stale, orphaning their live work (see `create_worktree`).
            remove_stale_registration_for_path(repo_root, &worktree_path);
            deleted.push(name.clone());
        }
    }
    deleted
}

/// Where a worktree directory is parked before it is deleted:
/// `<repo>/.worktrees/.trash` for anything under `.worktrees`, a sibling
/// `.trash` otherwise. Always on the directory's own volume, so getting
/// there is a plain rename.
fn trash_root(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    let root = path
        .ancestors()
        .skip(1)
        .find(|a| a.file_name().is_some_and(|n| n == ".worktrees"))
        .unwrap_or(parent);
    Some(root.join(".trash"))
}

/// Parked copies younger than this are left to whichever call parked them.
const TRASH_SWEEP_MIN_AGE: Duration = Duration::from_secs(300);

/// Atomically move `path` into a trash directory and return where it went,
/// leaving the delete to the caller. Tries `preferred_trash` first (it only
/// works on `path`'s own volume), then [`trash_root`]. On `Err`, `path` is
/// fully intact.
pub(crate) fn park_dir(
    path: &Path,
    name: &str,
    preferred_trash: Option<&Path>,
) -> Result<PathBuf, String> {
    let default_trash =
        trash_root(path).ok_or_else(|| format!("{} has no parent", path.display()))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let safe_name = name.replace(['/', '\\'], "-");
    let mut last_err = String::new();
    for trash in preferred_trash.into_iter().chain([default_trash.as_path()]) {
        if let Err(e) = std::fs::create_dir_all(trash) {
            last_err = format!("cannot create {}: {e}", trash.display());
            continue;
        }
        let mut parked = trash.join(format!("{safe_name}-{stamp}"));
        let mut n = 0;
        while parked.exists() {
            n += 1;
            parked = trash.join(format!("{safe_name}-{stamp}-{n}"));
        }
        for delay_ms in [100u64, 200, 400, 800, 1600, 0] {
            match std::fs::rename(path, &parked) {
                // A rename moves the whole tree or none of it; if the path is
                // still there, this one did not (a copy+delete fallback, or
                // someone recreated it). Delete nothing then.
                Ok(()) if path.exists() => {
                    return Err(format!(
                        "moving it aside left {} behind, nothing deleted (parked copy kept at {})",
                        path.display(),
                        parked.display()
                    ));
                }
                Ok(()) => return Ok(parked),
                Err(e) => {
                    last_err = e.to_string();
                    // No retry fixes a missing source or another volume.
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::CrossesDevices
                    ) {
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
        let _ = std::fs::remove_dir(trash);
    }
    Err(last_err)
}

/// Remove a worktree directory: rename it aside, then delete the copy.
///
/// Deleting in place is not atomic. On Windows any open handle under the
/// tree (a running test exe under `target\`, an editor, a git process)
/// makes `remove_dir_all` delete everything it can reach and then fail,
/// leaving a registered worktree with its branch intact and every tracked
/// file gone (item #689). Renaming the directory either moves the whole
/// tree or fails having touched nothing -- and it fails for exactly those
/// open handles. So the rename (retried past transient locks) is the only
/// step that can report failure, and it does so with `path` fully intact.
/// Once the tree is parked, `path` is free and deleting the parked copy is
/// best-effort. Reports the locking process via handle64.exe when present.
pub(super) fn remove_worktree_dir(path: &Path, name: &str) -> bool {
    let last_err = match park_dir(path, name, None) {
        Ok(parked) => {
            delete_parked(&parked);
            // Sweep copies an earlier call could not delete -- but not
            // fresh ones, which another call may be deleting right now --
            // then drop the trash directory itself once it is empty.
            if let Some(trash) = parked.parent() {
                for stale in std::fs::read_dir(trash).into_iter().flatten().flatten() {
                    let settled = stale
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > TRASH_SWEEP_MIN_AGE);
                    if settled {
                        let _ = std::fs::remove_dir_all(stale.path());
                    }
                }
                let _ = std::fs::remove_dir(trash);
            }
            return true;
        }
        Err(e) => e,
    };

    #[cfg(windows)]
    {
        for handle_exe in &["handle64.exe", "handle.exe"] {
            if let Ok(output) = flare_process::command(handle_exe)
                .args(["-accepteula", "-nobanner", &path.to_string_lossy()])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
            {
                if output.status.success() {
                    let out = String::from_utf8_lossy(&output.stdout);
                    for line in out.lines().take(5) {
                        eprintln!("worktree: locking process for '{}': {}", name, line);
                    }
                }
                break;
            }
        }
    }

    eprintln!("worktree: failed to remove orphan '{name}', left untouched: {last_err}");
    false
}

/// Best-effort delete of a directory already moved into the trash; a
/// leftover is swept by the next successful removal.
pub(crate) fn delete_parked(parked: &Path) {
    if std::fs::remove_dir_all(parked).is_ok() {
        return;
    }
    #[cfg(windows)]
    {
        let _ = flare_process::command("cmd")
            .args(["/c", "rmdir", "/s", "/q", &parked.to_string_lossy()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        // `rmdir`/`remove_dir_all` both fail identically against a genuine
        // ACL denial, not just a transient in-use lock -- item #267's actual
        // failure mode: cargo's own `target/*/.fingerprint/*` files can end
        // up ACL-restricted (not merely open), which no amount of retrying
        // or an in-use-lock-only tool like `rmdir` can clear. `icacls /grant
        // ... /T` resets ownership access recursively before one final
        // delete attempt.
        if parked.exists()
            && let Ok(user) = std::env::var("USERNAME")
        {
            let icacls_args: Vec<String> = vec![
                parked.to_string_lossy().to_string(),
                "/grant".to_string(),
                format!("{user}:F"),
                "/T".to_string(),
                "/C".to_string(),
                "/Q".to_string(),
            ];
            let _ = flare_process::command("icacls")
                .args(&icacls_args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
    if parked.exists()
        && let Err(e) = std::fs::remove_dir_all(parked)
    {
        eprintln!(
            "worktree: WARNING could not delete parked copy {} (the worktree path itself is \
             free; a later removal sweeps it): {e}",
            parked.display()
        );
    }
}

/// Recursive directory size in bytes.
pub(crate) fn dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::park_dir;
    use tempfile::TempDir;

    #[test]
    fn park_dir_moves_the_whole_tree_and_frees_the_path() {
        let root = TempDir::new().unwrap();
        let victim = root.path().join("proj").join("target");
        std::fs::create_dir_all(victim.join("debug")).unwrap();
        std::fs::write(victim.join("debug").join("a.o"), b"x").unwrap();
        let parked = park_dir(&victim, "target", None).unwrap();
        assert!(!victim.exists(), "original path must be free");
        assert!(
            parked.join("debug").join("a.o").exists(),
            "tree moved intact"
        );
        assert_eq!(
            parked.parent().unwrap(),
            root.path().join("proj").join(".trash")
        );
    }

    #[test]
    fn park_dir_prefers_the_given_trash_dir() {
        let root = TempDir::new().unwrap();
        let victim = root.path().join("proj").join("node_modules");
        std::fs::create_dir_all(&victim).unwrap();
        let trash = root.path().join(".worktrees").join(".trash");
        let parked = park_dir(&victim, "node_modules", Some(&trash)).unwrap();
        assert_eq!(parked.parent().unwrap(), trash);
    }

    #[test]
    fn park_dir_leaves_a_missing_path_as_an_error() {
        let root = TempDir::new().unwrap();
        let err = park_dir(&root.path().join("gone"), "gone", None);
        assert!(err.is_err());
    }
}
