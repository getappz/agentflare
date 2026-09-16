// Split out of `work.rs` to keep that file under the LOC gate.

/// Validates that `wpath` is enterable, then runs `f`.
///
/// This used to chdir the whole process into `wpath` around `f`, serialized
/// by a process-wide mutex: `std::env::set_current_dir` mutates the whole
/// PROCESS's cwd, not per-thread, and `WorkItemExecutor` dispatches jobs
/// concurrently (`work_max_concurrency`), so two jobs racing an unguarded
/// chdir here were observed live, twice, running one item's pipeline
/// against a *different* item's worktree. That's no longer a risk (item
/// #205): every git/agent spawn this pipeline reaches now takes its
/// directory explicitly instead of relying on the ambient cwd --
/// `agent_launch::run_headless_impl`'s `Command::current_dir`,
/// `flare_git_core::shell::run_in`/`diff`/`run_in_lines_bounded`, and
/// `worktree::{head_sha,commit_uncommitted_at,squash_since}` are all passed
/// the worktree path directly. Removing the chdir (and its lock) lets
/// concurrent dispatches actually run their pipelines in parallel, which is
/// what `work_max_concurrency` was already sized for.
///
/// Returns `Err` if `wpath` doesn't exist or isn't a directory; otherwise
/// `Ok` with whatever `f` returned.
fn run_in_worktree<T>(wpath: &std::path::Path, f: impl FnOnce() -> T) -> Result<T, String> {
    if !wpath.is_dir() {
        return Err(format!(
            "worktree path {} does not exist or is not a directory",
            wpath.display()
        ));
    }
    Ok(f())
}
