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
#[cfg(target_os = "linux")]
pub fn wrap(command: &str, args: &[String], cwd: Option<&Path>) -> (String, Vec<String>) {
    bwrap::wrap(command, args, cwd).unwrap_or_else(|| (command.to_string(), args.to_vec()))
}

#[cfg(not(target_os = "linux"))]
pub fn wrap(command: &str, args: &[String], _cwd: Option<&Path>) -> (String, Vec<String>) {
    (command.to_string(), args.to_vec())
}
