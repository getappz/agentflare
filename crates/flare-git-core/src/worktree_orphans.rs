//! Orphaned-worktree audit/GC and worktree-directory removal for
//! `worktree`. Child module of `worktree` (split out for size); public
//! items are re-exported from there, so `flare_git_core::worktree::X`
//! paths are unchanged.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{remove_stale_registration_for_path, resolve_gitdir_pointer};
use crate::shell::run_in as run_git_in;

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
        let dot_git = path.join(".git");
        let has_broken_gitdir = if dot_git.is_file() {
            match std::fs::read_to_string(&dot_git) {
                Ok(content) => {
                    let gitdir = content.trim().trim_start_matches("gitdir: ");
                    !resolve_gitdir_pointer(path, gitdir).exists()
                }
                Err(_) => false,
            }
        } else {
            false
        };
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
/// snapshots each one before removing it. Removal uses `remove_worktree_dir`
/// which retries with exponential backoff on Windows, falls back to `cmd /c
/// rmdir`, and reports locking processes via handle64.exe when present.
/// Returns the names actually deleted.
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

/// Remove a worktree directory, with retry + Windows fallback.
///
/// On Windows, background processes (rust-analyzer, proc-macro-srv) may
/// hold file handles that block `remove_dir_all` with Permission denied.
/// Retries with exponential backoff, falls back to `cmd /c rmdir`, and
/// attempts to identify the locking process via handle64.exe when present.
pub(super) fn remove_worktree_dir(path: &Path, name: &str) -> bool {
    let delays_ms = [100u64, 200, 400, 800, 1600];
    for delay in &delays_ms {
        if std::fs::remove_dir_all(path).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(*delay));
    }
    if std::fs::remove_dir_all(path).is_ok() {
        return true;
    }

    #[cfg(windows)]
    {
        if flare_process::command("cmd")
            .args(["/c", "rmdir", "/s", "/q", &path.to_string_lossy()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return true;
        }

        // `rmdir`/`remove_dir_all` both fail identically against a genuine
        // ACL denial, not just a transient in-use lock -- item #267's actual
        // failure mode: cargo's own `target/*/.fingerprint/*` files can end
        // up ACL-restricted (not merely open), which no amount of retrying
        // or an in-use-lock-only tool like `rmdir` can clear. `icacls /grant
        // ... /T` resets ownership access recursively before one final
        // delete attempt; a no-op (and thus never destructive) if the real
        // problem was actually an in-use lock the retries above already
        // would have cleared.
        if let Ok(user) = std::env::var("USERNAME") {
            let icacls_args: Vec<String> = vec![
                path.to_string_lossy().to_string(),
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
            if std::fs::remove_dir_all(path).is_ok() {
                return true;
            }
        }

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

    eprintln!(
        "worktree: failed to remove orphan '{}': Permission denied",
        name
    );
    false
}

/// Recursive directory size in bytes.
fn dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
        .sum()
}
