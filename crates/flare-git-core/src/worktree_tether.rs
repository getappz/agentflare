//! Run a subprocess tethered to a worktree directory: when the directory is
//! removed, tear down the child's whole process tree (item #810, #302).

use std::io::ErrorKind;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::process::terminate_tree_gracefully;

const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// `Some(true)` when `path` is gone, `Some(false)` when it still exists,
/// `None` on a transient stat error (must not trigger teardown).
#[must_use]
pub(crate) fn worktree_removal_detected(path: &Path) -> Option<bool> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Some(false),
        Err(e) if e.kind() == ErrorKind::NotFound => Some(true),
        Err(_) => None,
    }
}

/// Like `worktree_removal_detected`, but a root that still exists counts as removed once the
/// `.git` marker it had at spawn is gone. On Windows the child's cwd pins the root directory, so
/// removing the worktree deletes its contents yet leaves the empty directory behind.
fn root_removed(path: &Path, had_git_marker: bool) -> Option<bool> {
    match worktree_removal_detected(path) {
        // Only a definite "not there" counts; any other error leaves the answer open.
        Some(false) if had_git_marker => git_marker_present(path).map(|present| !present),
        other => other,
    }
}

/// `Some(false)` only when the `.git` entry is definitely absent; `None` on any other error.
fn git_marker_present(root: &Path) -> Option<bool> {
    match std::fs::symlink_metadata(root.join(".git")) {
        Ok(_) => Some(true),
        Err(e) if e.kind() == ErrorKind::NotFound || e.kind() == ErrorKind::NotADirectory => {
            Some(false)
        }
        Err(_) => None,
    }
}

fn spawn_tethered_child(
    worktree_root: &Path,
    program: &std::ffi::OsStr,
    args: &[&std::ffi::OsStr],
) -> Result<std::process::Child, String> {
    let mut cmd = flare_process::command(program);
    cmd.args(args)
        .current_dir(worktree_root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    crate::shell::apply_filtered_path(&mut cmd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }
    cmd.spawn()
        .map_err(|e| format!("tether: spawn {}: {e}", program.to_string_lossy()))
}

fn kill_if_running(child: &Arc<Mutex<Option<std::process::Child>>>) {
    if let Ok(mut guard) = child.lock() {
        if let Some(ref mut ch) = *guard {
            terminate_tree_gracefully(ch);
        }
        *guard = None;
    }
}

/// Runs `program` with `args` in `worktree_root`. When that directory is
/// removed from disk, kills the process tree. Returns the child's exit code.
///
/// # Errors
/// Spawn failures and wait errors.
pub fn run_tether(
    worktree_root: &Path,
    program: &std::ffi::OsStr,
    args: &[&std::ffi::OsStr],
) -> Result<i32, String> {
    let had_git_marker = git_marker_present(worktree_root) == Some(true);
    let child = spawn_tethered_child(worktree_root, program, args)?;

    let child_slot = Arc::new(Mutex::new(Some(child)));
    let finished = Arc::new(AtomicBool::new(false));
    let root = worktree_root.to_path_buf();
    let slot_reaper = Arc::clone(&child_slot);
    let finished_reaper = Arc::clone(&finished);
    let reaper = std::thread::spawn(move || {
        while !finished_reaper.load(Ordering::Relaxed) {
            match root_removed(&root, had_git_marker) {
                Some(true) => {
                    kill_if_running(&slot_reaper);
                    break;
                }
                Some(false) => {}
                None => {}
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    });

    let exit_code = loop {
        let mut guard = child_slot
            .lock()
            .map_err(|e| format!("tether: lock child: {e}"))?;
        let Some(ref mut ch) = *guard else {
            break 1;
        };
        match ch.try_wait() {
            Ok(Some(status)) => {
                *guard = None;
                break status.code().unwrap_or(1);
            }
            Ok(None) => {
                drop(guard);
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("tether: try_wait: {e}")),
        }
    };

    finished.store(true, Ordering::Relaxed);
    let _ = reaper.join();
    kill_if_running(&child_slot);
    Ok(exit_code)
}
