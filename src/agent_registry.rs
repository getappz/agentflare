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
}

impl Agent {
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
        }
    }
}

/// `Cli`-tier agents ship a standalone binary and are eligible for
/// PATH-based detection (this ticket) and, later, install/launch.
/// `Extension`-tier agents are editor-embedded (VS Code extensions) with no
/// independent binary to detect or launch — see issue #22's design doc.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Tier {
    Cli,
    Extension,
}

pub struct AgentSpec {
    pub id: Agent,
    pub display_name: &'static str,
    pub tier: Tier,
    /// Binary names to search for on PATH, in priority order. Empty for
    /// `Extension`-tier agents, which have no standalone binary.
    pub binary_names: &'static [&'static str],
    /// Arguments passed to the binary to print its version, e.g. `--version`.
    pub version_args: &'static [&'static str],
}

pub static REGISTRY: &[AgentSpec] = &[
    AgentSpec {
        id: Agent::ClaudeCode,
        display_name: "claude-code",
        tier: Tier::Cli,
        binary_names: &["claude"],
        version_args: &["--version"],
    },
    AgentSpec {
        id: Agent::Codex,
        display_name: "codex",
        tier: Tier::Cli,
        binary_names: &["codex"],
        version_args: &["--version"],
    },
    AgentSpec {
        id: Agent::Cursor,
        display_name: "cursor",
        tier: Tier::Cli,
        binary_names: &["cursor-agent"],
        version_args: &["--version"],
    },
    AgentSpec {
        id: Agent::Windsurf,
        display_name: "windsurf",
        tier: Tier::Cli,
        binary_names: &["windsurf"],
        version_args: &["--version"],
    },
    AgentSpec {
        id: Agent::Opencode,
        display_name: "opencode",
        tier: Tier::Cli,
        binary_names: &["opencode"],
        version_args: &["--version"],
    },
    AgentSpec {
        id: Agent::GeminiCli,
        display_name: "gemini-cli",
        tier: Tier::Cli,
        binary_names: &["gemini"],
        version_args: &["--version"],
    },
    AgentSpec {
        id: Agent::GithubCopilotCli,
        display_name: "github-copilot-cli",
        tier: Tier::Cli,
        binary_names: &["copilot"],
        version_args: &["--version"],
    },
    AgentSpec {
        id: Agent::Aider,
        display_name: "aider",
        tier: Tier::Cli,
        binary_names: &["aider"],
        version_args: &["--version"],
    },
    AgentSpec {
        id: Agent::VscodeCopilot,
        display_name: "vscode-copilot",
        tier: Tier::Extension,
        binary_names: &[],
        version_args: &[],
    },
    AgentSpec {
        id: Agent::Cline,
        display_name: "cline",
        tier: Tier::Extension,
        binary_names: &[],
        version_args: &[],
    },
    AgentSpec {
        id: Agent::Continue,
        display_name: "continue",
        tier: Tier::Extension,
        binary_names: &[],
        version_args: &[],
    },
];

pub fn spec(agent: Agent) -> &'static AgentSpec {
    REGISTRY
        .iter()
        .find(|s| s.id == agent)
        .expect("every Agent variant has exactly one REGISTRY entry")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_exactly_eleven_entries() {
        assert_eq!(REGISTRY.len(), 11);
    }

    #[test]
    fn registry_has_eight_cli_tier_and_three_extension_tier() {
        let cli_count = REGISTRY.iter().filter(|s| s.tier == Tier::Cli).count();
        let ext_count = REGISTRY.iter().filter(|s| s.tier == Tier::Extension).count();
        assert_eq!(cli_count, 8);
        assert_eq!(ext_count, 3);
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
    }

    #[test]
    fn as_str_matches_display_name_for_every_variant() {
        for s in REGISTRY {
            assert_eq!(s.id.as_str(), s.display_name);
        }
    }
}
