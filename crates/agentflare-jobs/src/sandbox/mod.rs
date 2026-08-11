//! Process-level sandboxing for job commands.
//!
//! Only Linux (native or WSL2) gets an enforced boundary today: bubblewrap is
//! built entirely on Linux kernel namespaces and has no Windows or macOS
//! equivalent, so those platforms keep running jobs exactly as they did
//! before this module existed.

#[cfg(target_os = "linux")]
mod bwrap;

use std::path::Path;

/// Returns the command/args that should actually be spawned for a job:
/// wrapped in a sandbox where one is available, or unchanged otherwise
/// (non-Linux platforms, or Linux without `bwrap` on `PATH`).
///
/// `git_writable`: pass `false` for an arbitrary job command (e.g. a
/// build/test/lint job dispatched via `Supervisor::spawn`), which has no
/// business rewriting git history -- `.git` stays read-only even though it
/// sits inside the job's otherwise-writable cwd. Pass `true` only for a
/// caller whose job IS to commit (the headless coding-agent CLI itself, see
/// `agent_launch::run_headless`) -- re-protecting `.git` read-only there
/// made every headless work-item dispatch unable to ever `git add`/`git
/// commit` its own staged changes (item #88).
#[cfg(target_os = "linux")]
pub fn wrap(
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    git_writable: bool,
) -> (String, Vec<String>) {
    bwrap::wrap(command, args, cwd, git_writable)
        .unwrap_or_else(|| (command.to_string(), args.to_vec()))
}

#[cfg(not(target_os = "linux"))]
pub fn wrap(
    command: &str,
    args: &[String],
    _cwd: Option<&Path>,
    _git_writable: bool,
) -> (String, Vec<String>) {
    (command.to_string(), args.to_vec())
}
