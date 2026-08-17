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
use std::path::Path;

const OVERLAY: MountPolicy = MountPolicy::OverlayEphemeral;

const fn mount(relative_path: &'static str) -> AgentStateMount {
    AgentStateMount {
        relative_path,
        policy: OVERLAY,
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
const OPENCODE_STATE: &[AgentStateMount] =
    &[mount(".config/opencode"), mount(".local/share/opencode")];

/// cursor-agent's own config/state dir. Holds `mcp.json`/`hooks.json` (read)
/// plus a per-project tracking directory cursor-agent creates on demand
/// under `.cursor/projects/<slug>` for whatever cwd it's launched against --
/// item #130, confirmed live: with `.cursor` left off the sandbox entirely,
/// that `mkdir` failed and every cursor-agent job died immediately with
/// "cursor exited non-zero -- ENOENT ... mkdir '.../.cursor/projects/<slug>'"
/// before doing any real work.
const CURSOR_STATE: &[AgentStateMount] = &[mount(".cursor")];

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
pub fn wrap(
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    git_writable: bool,
) -> (String, Vec<String>) {
    flare_sandbox::wrap(command, args, cwd, git_writable, &CONFIG)
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
        let (command, out_args) = wrap("cursor-agent", &args, Some(bogus_cwd), true);
        assert_eq!(command, "cursor-agent");
        assert_eq!(out_args, args);
    }
}
