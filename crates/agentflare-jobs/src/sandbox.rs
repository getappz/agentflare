//! agentflare's own sandboxing policy, layered on the agent-agnostic
//! `flare-sandbox` crate: which `$HOME` state directories each coding-agent
//! CLI needs mounted (and under what write policy), plus agentflare's own
//! `~/.agentflare` state dir which must stay writable regardless of which
//! agent is running.
//!
//! Per-agent paths are cross-referenced against `agent-registry`'s
//! `binary_names` (the actual resolved CLI binary each profile matches) and,
//! where a convention is already known, akitaonrails/ai-jail's own
//! `command_state_paths` table -- adopting *which* directories each agent
//! needs, not its write policy (ai-jail persists these with a real
//! read-write bind; agentflare deliberately keeps every agent-state write
//! ephemeral, see `OverlayEphemeral`'s doc comment).

use flare_sandbox::{AgentProfile, AgentStateMount, MountPolicy, SandboxConfig};
use std::path::{Path, PathBuf};

const OVERLAY: MountPolicy = MountPolicy::OverlayEphemeral;

const fn mount(relative_path: &'static str) -> AgentStateMount {
    AgentStateMount {
        relative_path,
        policy: OVERLAY,
        diagnostic_log: None,
    }
}

/// Like [`mount`], but names a log file (relative to `relative_path`) worth
/// tailing out before the sandbox tears down its `OverlayEphemeral` overlay
/// -- see `flare_sandbox::AgentStateMount::diagnostic_log`'s doc comment and
/// item #139.
const fn mount_with_diagnostic_log(
    relative_path: &'static str,
    diagnostic_log: &'static str,
) -> AgentStateMount {
    AgentStateMount {
        relative_path,
        policy: OVERLAY,
        diagnostic_log: Some(diagnostic_log),
    }
}

/// claude-code's own data dir. Holds `.credentials.json`, which claude-code's
/// OAuth refresh machinery both reads (existing access/refresh tokens) and
/// writes (persisting the rotated token pair after a successful server-side
/// refresh). The write side is what the sandbox's read-only root breaks:
/// claude-code's refresh flow first creates `.oauth_refresh.lock` (inside
/// this dir) and later rewrites the credentials file via a temp-file+rename,
/// and both fail with EROFS when the dir falls through to the read-only
/// root -- the refresh never completes and the job dies with "Failed to
/// authenticate. API Error: 401 OAuth access token has expired" against the
/// stale token (item #127, confirmed live: every sandboxed job hit it while
/// the same token refreshed fine interactively).
const CLAUDE_STATE: &[AgentStateMount] = &[mount(".claude")];

/// opencode's own dirs -- unlike claude-code/codex/gemini's headless modes,
/// `opencode run` unconditionally writes into `.local/share/opencode` on
/// every invocation (an append-mode log file, plus a SQLite session DB it
/// checkpoints), which crashes outright under the read-only root (item
/// #106: `FileSystem.open` on `opencode.log`, then a WAL checkpoint failure
/// once that's worked around). It also holds `auth.json`, so it can't just
/// go read-only. `.config/opencode` (MCP/tool config) is mounted alongside
/// it, matching ai-jail's own `command_state_paths` coverage for opencode.
const OPENCODE_STATE: &[AgentStateMount] = &[
    mount(".config/opencode"),
    mount_with_diagnostic_log(".local/share/opencode", "log/opencode.log"),
];

/// cursor-agent's own config/state dirs. `.cursor` holds `mcp.json`/
/// `hooks.json` (read) plus a per-project tracking directory cursor-agent
/// creates on demand under `.cursor/projects/<slug>` for whatever cwd it's
/// launched against -- item #130, confirmed live: with `.cursor` left off
/// the sandbox entirely, that `mkdir` failed and every cursor-agent job died
/// immediately with "cursor exited non-zero -- ENOENT ... mkdir
/// '.../.cursor/projects/<slug>'" before doing any real work.
///
/// `.config/cursor` is a *separate* dir cursor-agent also writes to
/// unconditionally in `-p`/headless mode: `auth.json` plus a per-chat
/// session directory under `.config/cursor/chats/<id>/<id>` it creates on
/// every dispatch. Confirmed live: with only `.cursor` mounted, that
/// `mkdir` failed with "RetriableError: [internal] ENOENT: no such file or
/// directory, mkdir '.../.config/cursor/chats/<id>/<id>'", which
/// cursor-agent treats as retriable and loops reconnecting to
/// `api5.cursor.sh` until the job times out -- every headless cursor-agent
/// job died this way despite `.cursor` already being mounted.
const CURSOR_STATE: &[AgentStateMount] = &[mount(".cursor"), mount(".config/cursor")];

const CODEX_STATE: &[AgentStateMount] = &[mount(".codex")];
const GEMINI_STATE: &[AgentStateMount] = &[mount(".gemini")];
const AIDER_STATE: &[AgentStateMount] = &[mount(".aider")];
const GROK_STATE: &[AgentStateMount] = &[mount(".grok")];
const KIMI_STATE: &[AgentStateMount] = &[mount(".kimi-code")];

/// Every coding-agent CLI agentflare knows a `$HOME` state-dir convention
/// for, matched against `agent-registry`'s `binary_names`. An agent not
/// listed here (e.g. windsurf, copilot, cody, goose, amp, kiro, antigravity,
/// openclaw, droid) simply gets no agent-specific mount -- not a bug, just
/// nothing has hit the failure mode yet that would tell us what it needs.
const AGENT_PROFILES: &[AgentProfile] = &[
    AgentProfile {
        binary_name: "claude",
        state_mounts: CLAUDE_STATE,
    },
    AgentProfile {
        binary_name: "opencode",
        state_mounts: OPENCODE_STATE,
    },
    AgentProfile {
        binary_name: "cursor-agent",
        state_mounts: CURSOR_STATE,
    },
    AgentProfile {
        binary_name: "codex",
        state_mounts: CODEX_STATE,
    },
    AgentProfile {
        binary_name: "gemini",
        state_mounts: GEMINI_STATE,
    },
    AgentProfile {
        binary_name: "aider",
        state_mounts: AIDER_STATE,
    },
    AgentProfile {
        binary_name: "grok",
        state_mounts: GROK_STATE,
    },
    AgentProfile {
        binary_name: "kimi",
        state_mounts: KIMI_STATE,
    },
];

/// The agentflare MCP server's own state dir, relative to `$HOME` -- holds
/// `agentflare.db`, the sqlite store every `item`/`comment`/`vent` MCP call
/// writes through. Unlike agent state mounts, this one must persist: a
/// dispatched job's whole job-completion signal (`item action=done`,
/// `comment action=create`, even `vent` for reporting a sandbox problem like
/// this one) is an MCP call into this same DB, so an ephemeral overlay here
/// means the agent finishes real work with no way to report it (item #120 --
/// confirmed live via `EROFS` on `touch ~/.agentflare/probe` and two
/// dispatched jobs stuck showing not-done despite merge-ready PRs).
const WRITABLE_HOME_DIRS: &[&str] = &[".agentflare"];

const CONFIG: SandboxConfig = SandboxConfig {
    agent_profiles: AGENT_PROFILES,
    writable_home_dirs: WRITABLE_HOME_DIRS,
};

/// `.agentflare`-relative directory a sandboxed run's diagnostic-log tail
/// (see `OPENCODE_STATE`'s `diagnostic_log`) gets written to -- nested under
/// `.agentflare` because that's already a real, host-persistent writable
/// bind (`WRITABLE_HOME_DIRS`), so no additional bind is needed.
const DIAGNOSTIC_SUBDIR: &str = ".agentflare/sandbox-diagnostics";

/// Absolute host path a sandboxed run's diagnostic-log tail (if any) would
/// be written to for the given unique `token`. The caller creates this
/// path's parent directory before spawning (so the bind exists even on a
/// box's very first sandboxed run) and reads + removes it after the child
/// exits -- see `agent_launch::run_headless_impl`. `None` if `$HOME` can't
/// be resolved, mirroring `wrap`'s own fallback when sandboxing is
/// unavailable.
pub fn diagnostic_path(token: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        Path::new(&home)
            .join(DIAGNOSTIC_SUBDIR)
            .join(format!("{token}.log")),
    )
}

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
///
/// `diagnostic_out`: forwarded to `flare_sandbox::wrap` -- see its doc
/// comment. Pass `None` for a job whose own output has no diagnostic value
/// beyond stdout/stderr (e.g. `Supervisor::spawn`'s arbitrary build/lint/
/// test commands); pass `diagnostic_path(token)` for a headless coding-agent
/// CLI dispatch (`agent_launch::run_headless`) so a failure is diagnosable
/// from more than a short stdout/stderr tail (item #139).
pub fn wrap(
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    git_writable: bool,
    diagnostic_out: Option<&Path>,
) -> (String, Vec<String>) {
    flare_sandbox::wrap(command, args, cwd, git_writable, &CONFIG, diagnostic_out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn agent_profiles_have_unique_binary_names() {
        let mut seen = HashSet::new();
        for profile in AGENT_PROFILES {
            assert!(
                seen.insert(profile.binary_name),
                "duplicate agent profile for {}",
                profile.binary_name
            );
        }
    }

    #[test]
    fn agent_profiles_have_at_least_one_state_mount() {
        for profile in AGENT_PROFILES {
            assert!(
                !profile.state_mounts.is_empty(),
                "{} has no state mounts -- remove the profile instead of leaving it empty",
                profile.binary_name
            );
        }
    }

    #[test]
    fn wrap_falls_back_to_plain_command_when_cwd_does_not_exist() {
        // `wrap` resolves cwd before sandboxing (see flare-sandbox::wrap's
        // doc comment); an unresolvable path must fall back to running the
        // job unchanged, regardless of whether bwrap is installed on this
        // machine -- keeps the test deterministic across CI runners.
        let bogus_cwd = Path::new("/definitely-does-not-exist-agentflare-sandbox-test");
        let args = vec!["--help".to_string()];
        let (command, out_args) = wrap("cursor-agent", &args, Some(bogus_cwd), true, None);
        assert_eq!(command, "cursor-agent");
        assert_eq!(out_args, args);
    }

    #[test]
    fn diagnostic_path_is_nested_under_agentflare_home_dir() {
        let path = diagnostic_path("some-token").expect("HOME is set in test environment");
        assert!(path.ends_with(".agentflare/sandbox-diagnostics/some-token.log"));
    }

    #[test]
    fn opencode_state_names_a_diagnostic_log_for_its_data_dir_mount() {
        // Item #139: opencode's own tool-call/reasoning trace lives in
        // `log/opencode.log` under `.local/share/opencode`, which otherwise
        // vanishes entirely with the rest of that mount's discarded overlay.
        let data_dir_mount = OPENCODE_STATE
            .iter()
            .find(|m| m.relative_path == ".local/share/opencode")
            .expect(".local/share/opencode mount present");
        assert_eq!(data_dir_mount.diagnostic_log, Some("log/opencode.log"));
    }
}
