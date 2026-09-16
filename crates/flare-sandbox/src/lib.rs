//! Reusable, configurable process sandboxing for AI coding-agent CLIs.
//!
//! Linux/WSL2 gets an enforced boundary via bubblewrap (`bwrap`): built
//! entirely on Linux kernel namespaces, with no Windows or macOS
//! equivalent, so those platforms pass commands through unchanged.
//!
//! This crate has no built-in knowledge of any specific agent CLI -- a
//! caller supplies a [`SandboxConfig`] describing which `$HOME` state
//! directories each agent needs mounted (and under what write policy), plus
//! any directories of its own that should stay writable regardless of which
//! agent is running.

#[cfg(target_os = "linux")]
mod bwrap;

use std::path::Path;

/// How a mounted `$HOME` state directory's writes are handled.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountPolicy {
    /// Reads see the real directory; writes land in a discarded tmpfs
    /// overlay and never persist back to the host. The default containment
    /// policy for agent auth/session state -- a sandboxed job's oauth
    /// refresh or per-project tracking file shouldn't outlive that one job.
    OverlayEphemeral,
}

/// One `$HOME`-relative directory an agent CLI needs mounted into the
/// sandbox beyond the generic read-only build/package caches (`.cargo`,
/// `.npm`, ...), which every sandboxed job gets regardless of profile.
#[derive(Debug, Clone, Copy)]
pub struct AgentStateMount {
    pub relative_path: &'static str,
    pub policy: MountPolicy,
    /// Path, relative to this mount's own directory, of a log file worth
    /// tailing out to the caller's `diagnostic_out` path (see [`wrap`])
    /// before the sandboxed process exits -- otherwise this mount's entire
    /// `OverlayEphemeral` overlay, log included, is discarded with it,
    /// leaving a failed headless run undiagnosable (item #139). `None` when
    /// this agent has no known diagnostic log.
    pub diagnostic_log: Option<&'static str>,
}

/// Sandbox policy for one agent CLI: which resolved binary this applies to
/// (matched on the command's final path component, e.g. `"claude"` or
/// `"cursor-agent"`) and which of its `$HOME` state directories to mount.
#[derive(Debug, Clone, Copy)]
pub struct AgentProfile {
    pub binary_name: &'static str,
    pub state_mounts: &'static [AgentStateMount],
}

/// A caller's sandboxing policy: known agent CLIs (see [`AgentProfile`]),
/// plus any `$HOME`-relative directories that must stay writable no matter
/// which agent is running -- unlike agent state mounts, writes here persist
/// to the host (e.g. a caller's own on-disk state a dispatched job reports
/// completion through).
#[derive(Debug, Clone, Copy, Default)]
pub struct SandboxConfig {
    pub agent_profiles: &'static [AgentProfile],
    pub writable_home_dirs: &'static [&'static str],
}

/// Returns the command/args that should actually be spawned for a job:
/// wrapped in a sandbox where one is available, or unchanged otherwise
/// (non-Linux platforms, or Linux without `bwrap` on `PATH`).
///
/// `git_writable`: pass `false` for an arbitrary job command (e.g. a
/// build/test/lint job), which has no business rewriting git history --
/// `.git` stays read-only even though it sits inside the job's otherwise-
/// writable cwd. Pass `true` only for a caller whose job IS to commit (a
/// headless coding-agent CLI itself) -- re-protecting `.git` read-only
/// there would leave every staged/committed change stranded.
///
/// `diagnostic_out`: when `Some`, and the resolved `command` matches a
/// profile with a mount whose `diagnostic_log` is set, a bounded tail of
/// that log is periodically snapshotted to this (real, host-visible) path
/// for as long as the sandboxed command runs -- surviving even a hard
/// kill, not just a clean exit (item #139). `None` (or no match) leaves the
/// sandboxed command unwrapped, same as before this parameter existed.
#[cfg(target_os = "linux")]
pub fn wrap(
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    git_writable: bool,
    config: &SandboxConfig,
    diagnostic_out: Option<&Path>,
) -> (String, Vec<String>) {
    bwrap::wrap(command, args, cwd, git_writable, config, diagnostic_out)
        .unwrap_or_else(|| (command.to_string(), args.to_vec()))
}

#[cfg(not(target_os = "linux"))]
pub fn wrap(
    command: &str,
    args: &[String],
    _cwd: Option<&Path>,
    _git_writable: bool,
    _config: &SandboxConfig,
    _diagnostic_out: Option<&Path>,
) -> (String, Vec<String>) {
    (command.to_string(), args.to_vec())
}
