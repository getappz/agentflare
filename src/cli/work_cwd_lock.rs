// Split out of `work.rs` to keep that file under the LOC gate.

/// Serializes the "chdir into the item's worktree -> run `f` -> restore
/// cwd" span against concurrent dispatch. `set_current_dir` mutates the
/// whole PROCESS's cwd, not per-thread, but `WorkItemExecutor` dispatches
/// jobs concurrently (`work_max_concurrency`) -- without this lock, two
/// jobs racing here can have one item's pipeline run against a *different*
/// item's worktree (observed live, twice, with two different item pairs).
/// Same mitigation shape as `flare_git_core::worktree::WORKTREE_ADD_LOCK`;
/// only this span is serialized, not the whole worker pool.
///
/// Returns `Err` if `wpath` itself can't be entered; otherwise `Ok` with
/// whatever `f` returned.
fn run_in_worktree<T>(wpath: &std::path::Path, f: impl FnOnce() -> T) -> Result<T, String> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    // Contended case only (uncontended is the common path and would just be
    // noise): this span can run for minutes to hours (it wraps the whole
    // headless agent turn), so a queued item's dispatch thread can otherwise
    // sit silent for that entire time with no way to tell "waiting on
    // another item's turn" apart from "actually hung" -- exactly the
    // ambiguity that made a real, still-running dispatch look indistinguishable
    // from a stuck one from the daemon logs / per-job log file alone.
    let _guard = match LOCK.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            let waited_since = std::time::Instant::now();
            eprintln!(
                "agentflare-work: waiting for worktree-cwd lock -- another dispatched item is still running its pipeline, {} is queued behind it",
                wpath.display()
            );
            let guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
            eprintln!(
                "agentflare-work: acquired worktree-cwd lock for {} after waiting {:?}",
                wpath.display(),
                waited_since.elapsed()
            );
            guard
        }
    };
    let original_dir = std::env::current_dir().ok();
    if std::env::set_current_dir(wpath).is_err() {
        return Err(format!("failed to chdir into {}", wpath.display()));
    }
    let result = f();
    if let Some(d) = original_dir {
        let _ = std::env::set_current_dir(d);
    }
    Ok(result)
}
