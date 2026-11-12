// Canonical registry of AI coding agents agentflare knows about. `Agent` is
// the identifier shared by `init --agent`, `hook ... --agent`, and the
// `agents` detection subcommand — one enum, one source of truth, instead of
// each feature growing its own agent list.
use clap::ValueEnum;

#[derive(Copy, Clone, ValueEnum, Debug, PartialEq, Eq)]
#[value(rename_all = "kebab-case")]
pub enum Agent {
    ClaudeCode,
    Codex,
    Cursor,
    Windsurf,
    VscodeCopilot,
    Cline,
    Continue,
    Opencode,
    GeminiCli,
    GithubCopilotCli,
    Aider,
    Cody,
    Goose,
    Amp,
    Kiro,
    Antigravity,
    Grok,
    Kimi,
    Openclaw,
    Droid,
}

impl Agent {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
            Agent::Cursor => "cursor",
            Agent::Windsurf => "windsurf",
            Agent::VscodeCopilot => "vscode-copilot",
            Agent::Cline => "cline",
            Agent::Continue => "continue",
            Agent::Opencode => "opencode",
            Agent::GeminiCli => "gemini-cli",
            Agent::GithubCopilotCli => "github-copilot-cli",
            Agent::Aider => "aider",
            Agent::Cody => "cody",
            Agent::Goose => "goose",
            Agent::Amp => "amp",
            Agent::Kiro => "kiro",
            Agent::Antigravity => "antigravity",
            Agent::Grok => "grok",
            Agent::Kimi => "kimi",
            Agent::Openclaw => "openclaw",
            Agent::Droid => "droid",
        }
    }
}

/// Maps common human-typed aliases to the canonical registry name (e.g.
/// `"claude"` -> `"claude-code"`), so any consumer that stores or matches
/// agent names by exact string — handoff recipients, item assignees —
/// shares one alias table instead of each feature growing its own.
/// Unrecognized input passes through unchanged: these fields accept
/// arbitrary agent names, not just ones in `REGISTRY`. Add more arms as
/// aliases show up.
#[must_use]
pub fn canonicalize(name: &str) -> String {
    match name.trim().to_ascii_lowercase().as_str() {
        "claude" | "claude code" | "claude-code-cli" => Agent::ClaudeCode.as_str().to_string(),
        "agy" | "antigravity-cli" => Agent::Antigravity.as_str().to_string(),
        _ => name.to_string(),
    }
}

/// Resolves a user-typed agent name to its canonical `Agent`, accepting the
/// same aliases `canonicalize` does (e.g. `"claude"` -> `ClaudeCode`). The
/// one place every agent-name-string entry point (`--agent`, an item's
/// `assignee_agent`, a router config's `default`/`use`) should resolve
/// through, so an alias behaves identically no matter which one it came in.
#[must_use]
pub fn agent_by_name(name: &str) -> Option<Agent> {
    let canonical = canonicalize(name);
    REGISTRY
        .iter()
        .find(|s| s.id.as_str() == canonical)
        .map(|s| s.id)
}

/// `Cli`-tier agents ship a standalone binary and are eligible for
/// PATH-based detection (this ticket) and, later, install/launch.
/// `Extension`-tier agents are editor-embedded (VS Code extensions) with no
/// independent binary to detect or launch — see issue #22's design doc.
/// Not yet consumed outside tests — wired up by the upcoming agent detection
/// engine and CLI commands.
#[allow(dead_code)]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tier {
    Cli,
    Extension,
}

/// Not yet consumed outside tests — wired up by the upcoming agent detection
/// engine and CLI commands.
#[allow(dead_code)]
pub struct AgentSpec {
    pub id: Agent,
    pub display_name: &'static str,
    pub tier: Tier,
    /// Binary names to search for on PATH, in priority order. Empty for
    /// `Extension`-tier agents, which have no standalone binary.
    pub binary_names: &'static [&'static str],
    /// Arguments passed to the binary to print its version, e.g. `--version`.
    pub version_args: &'static [&'static str],
    /// Package manager for install/update/uninstall. `None` means no
    /// automated install path (e.g. Cursor requires a manual download).
    pub package_manager: Option<&'static str>,
    /// Package name for the package manager (npm scope, pip package, etc.).
    pub package_name: Option<&'static str>,
}

/// Not yet consumed outside tests — wired up by the upcoming agent detection
/// engine and CLI commands.
#[allow(dead_code)]
pub static REGISTRY: &[AgentSpec] = &[
    AgentSpec {
        id: Agent::ClaudeCode,
        display_name: "claude-code",
        tier: Tier::Cli,
        binary_names: &["claude"],
        version_args: &["--version"],
        package_manager: Some("npm"),
        package_name: Some("@anthropic-ai/claude-code"),
    },
    AgentSpec {
        id: Agent::Codex,
        display_name: "codex",
        tier: Tier::Cli,
        binary_names: &["codex"],
        version_args: &["--version"],
        package_manager: Some("npm"),
        package_name: Some("@openai/codex"),
    },
    AgentSpec {
        id: Agent::Cursor,
        display_name: "cursor",
        tier: Tier::Cli,
        binary_names: &["cursor-agent"],
        version_args: &["--version"],
        package_manager: None,
        package_name: None,
    },
    AgentSpec {
        id: Agent::Windsurf,
        display_name: "windsurf",
        tier: Tier::Cli,
        binary_names: &["windsurf"],
        version_args: &["--version"],
        package_manager: None,
        package_name: None,
    },
    AgentSpec {
        id: Agent::Opencode,
        display_name: "opencode",
        tier: Tier::Cli,
        binary_names: &["opencode"],
        version_args: &["--version"],
        package_manager: Some("npm"),
        package_name: Some("@opencode-ai/opencode"),
    },
    AgentSpec {
        id: Agent::GeminiCli,
        display_name: "gemini-cli",
        tier: Tier::Cli,
        binary_names: &["gemini"],
        version_args: &["--version"],
        package_manager: Some("npm"),
        package_name: Some("@google/gemini-cli"),
    },
    AgentSpec {
        id: Agent::GithubCopilotCli,
        display_name: "github-copilot-cli",
        tier: Tier::Cli,
        binary_names: &["copilot"],
        version_args: &["--version"],
        package_manager: Some("npm"),
        package_name: Some("@github/copilot-cli"),
    },
    AgentSpec {
        id: Agent::Aider,
        display_name: "aider",
        tier: Tier::Cli,
        binary_names: &["aider"],
        version_args: &["--version"],
        package_manager: Some("pip"),
        package_name: Some("aider-chat"),
    },
    AgentSpec {
        id: Agent::Cody,
        display_name: "cody",
        tier: Tier::Cli,
        binary_names: &["cody"],
        version_args: &["--version"],
        package_manager: Some("npm"),
        package_name: Some("@sourcegraph/cody"),
    },
    AgentSpec {
        id: Agent::Goose,
        display_name: "goose",
        tier: Tier::Cli,
        binary_names: &["goose"],
        version_args: &["--version"],
        package_manager: Some("npm"),
        package_name: Some("goose"),
    },
    AgentSpec {
        id: Agent::Amp,
        display_name: "amp",
        tier: Tier::Cli,
        binary_names: &["amp"],
        version_args: &["--version"],
        package_manager: None,
        package_name: None,
    },
    AgentSpec {
        id: Agent::Kiro,
        display_name: "kiro",
        tier: Tier::Cli,
        binary_names: &["kiro"],
        version_args: &["--version"],
        package_manager: None,
        package_name: None,
    },
    AgentSpec {
        id: Agent::Antigravity,
        display_name: "antigravity",
        tier: Tier::Cli,
        binary_names: &["antigravity", "agy"],
        version_args: &["--version"],
        package_manager: None,
        package_name: None,
    },
    AgentSpec {
        id: Agent::Grok,
        display_name: "grok",
        tier: Tier::Cli,
        binary_names: &["grok"],
        version_args: &["--version"],
        package_manager: None,
        package_name: None,
    },
    AgentSpec {
        id: Agent::Kimi,
        display_name: "kimi",
        tier: Tier::Cli,
        binary_names: &["kimi"],
        version_args: &["--version"],
        package_manager: None,
        package_name: None,
    },
    AgentSpec {
        id: Agent::Openclaw,
        display_name: "openclaw",
        tier: Tier::Cli,
        binary_names: &["openclaw"],
        version_args: &["--version"],
        package_manager: None,
        package_name: None,
    },
    AgentSpec {
        id: Agent::Droid,
        display_name: "droid",
        tier: Tier::Cli,
        binary_names: &["droid"],
        version_args: &["--version"],
        package_manager: None,
        package_name: None,
    },
    AgentSpec {
        id: Agent::VscodeCopilot,
        display_name: "vscode-copilot",
        tier: Tier::Extension,
        binary_names: &[],
        version_args: &[],
        package_manager: None,
        package_name: None,
    },
    AgentSpec {
        id: Agent::Cline,
        display_name: "cline",
        tier: Tier::Cli,
        binary_names: &["cline"],
        version_args: &["--version"],
        package_manager: Some("npm"),
        package_name: Some("cline"),
    },
    AgentSpec {
        id: Agent::Continue,
        display_name: "continue",
        tier: Tier::Extension,
        binary_names: &[],
        version_args: &[],
        package_manager: None,
        package_name: None,
    },
];

/// Not yet consumed outside tests — wired up by the upcoming agent detection
/// engine and CLI commands.
///
/// # Panics
///
/// Panics if `agent` has no `REGISTRY` entry — every `Agent` variant is
/// guaranteed exactly one by the `registry_has_exactly_twenty_entries` test.
#[allow(dead_code)]
#[must_use]
pub fn spec(agent: Agent) -> &'static AgentSpec {
    REGISTRY
        .iter()
        .find(|s| s.id == agent)
        .expect("every Agent variant has exactly one REGISTRY entry")
}

/// Nearest-capability ClinePass model for a Claude model name/alias — used
/// when the router picks `Agent::Cline` for a task whose configured `model`
/// is a Claude name (e.g. a `[router]` rule written before Cline was wired
/// up, or shared across rules with other agents).
///
/// ClinePass ($9.99/mo, confirmed via docs.cline.bot/getting-started/clinepass)
/// carries only open-weight models — no Anthropic models at all, so there is
/// no literal equivalent. This maps each Claude family to the ClinePass model
/// closest to it in capability, judged by ClinePass's own reference per-token
/// pricing (higher price ≈ higher capability tier, same convention Anthropic
/// uses across its own Opus/Sonnet/Haiku lineup): Fable and Opus (Anthropic's
/// two priciest/highest tiers) map to ClinePass's two priciest models, Sonnet
/// maps to Kimi K2.7's coding-specialized variant (Sonnet is the SDD loop's
/// usual implementer tier, so a coding-specialized pick outweighs a strict
/// price match), and Haiku maps to ClinePass's cheapest/fastest model. This
/// is a heuristic, not a vendor-published equivalence — no such table exists.
#[allow(dead_code)]
#[must_use]
pub fn clinepass_model_for_claude(model: &str) -> Option<&'static str> {
    let lower = model.trim().to_ascii_lowercase();
    if lower.contains("fable") {
        Some("cline-pass/kimi-k3")
    } else if lower.contains("opus") {
        Some("cline-pass/qwen3.7-max")
    } else if lower.contains("sonnet") {
        Some("cline-pass/kimi-k2.7-code")
    } else if lower.contains("haiku") {
        Some("cline-pass/deepseek-v4-flash")
    } else {
        None
    }
}

/// The flag that carries `agentflare run --mode <m>` / `agents launch --mode
/// <m>` to an interactive launch. Claude Code spells it `--permission-mode`
/// (confirmed via `claude --help`). Codex maps mode to `--sandbox`.
/// Other agents keep the historical pass-through `--mode` until confirmed.
#[must_use]
pub fn mode_flag(agent: Agent) -> &'static str {
    match agent {
        Agent::ClaudeCode => "--permission-mode",
        Agent::Codex => "--sandbox",
        _ => "--mode",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_exactly_twenty_entries() {
        assert_eq!(REGISTRY.len(), 20);
    }

    #[test]
    fn registry_has_eighteen_cli_tier_and_two_extension_tier() {
        let cli_count = REGISTRY.iter().filter(|s| s.tier == Tier::Cli).count();
        let ext_count = REGISTRY
            .iter()
            .filter(|s| s.tier == Tier::Extension)
            .count();
        assert_eq!(cli_count, 18);
        assert_eq!(ext_count, 2);
    }

    #[test]
    fn cli_tier_entries_have_at_least_one_binary_name() {
        for s in REGISTRY.iter().filter(|s| s.tier == Tier::Cli) {
            assert!(
                !s.binary_names.is_empty(),
                "{} is Tier::Cli but has no binary_names",
                s.display_name
            );
        }
    }

    #[test]
    fn extension_tier_entries_have_no_binary_names() {
        for s in REGISTRY.iter().filter(|s| s.tier == Tier::Extension) {
            assert!(
                s.binary_names.is_empty(),
                "{} is Tier::Extension but has binary_names",
                s.display_name
            );
        }
    }

    #[test]
    fn spec_looks_up_the_matching_entry() {
        assert_eq!(spec(Agent::Aider).display_name, "aider");
        assert_eq!(spec(Agent::VscodeCopilot).tier, Tier::Extension);
        assert_eq!(spec(Agent::Cody).display_name, "cody");
    }

    #[test]
    fn as_str_matches_display_name_for_every_variant() {
        for s in REGISTRY {
            assert_eq!(s.id.as_str(), s.display_name);
        }
    }

    #[test]
    fn clinepass_model_for_claude_maps_every_known_family() {
        assert_eq!(
            clinepass_model_for_claude("claude-fable-5"),
            Some("cline-pass/kimi-k3")
        );
        assert_eq!(
            clinepass_model_for_claude("claude-opus-4-6"),
            Some("cline-pass/qwen3.7-max")
        );
        assert_eq!(
            clinepass_model_for_claude("sonnet"),
            Some("cline-pass/kimi-k2.7-code")
        );
        assert_eq!(
            clinepass_model_for_claude("claude-haiku-4-5-20251001"),
            Some("cline-pass/deepseek-v4-flash")
        );
    }

    #[test]
    fn clinepass_model_for_claude_none_for_unrecognized_name() {
        assert_eq!(clinepass_model_for_claude("gpt-4o"), None);
        assert_eq!(clinepass_model_for_claude("cline-pass/kimi-k3"), None);
    }

    #[test]
    fn mode_flag_maps_codex_sandbox_and_claude_permissions() {
        assert_eq!(mode_flag(Agent::ClaudeCode), "--permission-mode");
        assert_eq!(mode_flag(Agent::Codex), "--sandbox");
        assert_eq!(mode_flag(Agent::Cursor), "--mode");
    }

    #[test]
    fn canonicalize_maps_known_aliases_and_passes_through_unknown() {
        assert_eq!(canonicalize("claude"), "claude-code");
        assert_eq!(canonicalize("Claude Code"), "claude-code");
        assert_eq!(canonicalize("  CLAUDE-CODE-CLI  "), "claude-code");
        assert_eq!(canonicalize("agy"), "antigravity");
        assert_eq!(canonicalize("antigravity-cli"), "antigravity");
        assert_eq!(canonicalize("codex"), "codex");
        assert_eq!(canonicalize("some-unknown-agent"), "some-unknown-agent");
    }

    #[test]
    fn agent_by_name_resolves_aliases_same_as_canonicalize() {
        assert_eq!(agent_by_name("claude"), Some(Agent::ClaudeCode));
        assert_eq!(agent_by_name("Claude Code"), Some(Agent::ClaudeCode));
        assert_eq!(agent_by_name("agy"), Some(Agent::Antigravity));
        assert_eq!(agent_by_name("antigravity-cli"), Some(Agent::Antigravity));
        assert_eq!(agent_by_name("codex"), Some(Agent::Codex));
        assert_eq!(agent_by_name("not-a-real-agent"), None);
    }
}
