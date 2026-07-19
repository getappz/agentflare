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
        binary_names: &["antigravity"],
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
        tier: Tier::Extension,
        binary_names: &[],
        version_args: &[],
        package_manager: None,
        package_name: None,
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
#[allow(dead_code)]
#[must_use]
pub fn spec(agent: Agent) -> &'static AgentSpec {
    REGISTRY
        .iter()
        .find(|s| s.id == agent)
        .expect("every Agent variant has exactly one REGISTRY entry")
}

/// The subcommand/flags that put an agent's CLI into non-interactive "print"
/// mode, to be followed by the prompt as the final argument (e.g. `claude -p
/// "<prompt>"`, `codex exec "<prompt>"`). `None` for agents with no headless
/// invocation (editor-embedded, or simply not yet mapped).
#[allow(dead_code)]
#[must_use]
pub fn headless_args(agent: Agent) -> Option<&'static [&'static str]> {
    match agent {
        Agent::ClaudeCode => Some(&["-p"]),
        Agent::Codex => Some(&["exec"]),
        Agent::GeminiCli => Some(&["-p"]),
        Agent::Opencode => Some(&["run"]),
        _ => None,
    }
}

/// Permission-bypass / autonomy flags for agents in headless mode, appended
/// after the print-mode flags and before the prompt (e.g., `claude -p
/// --dangerously-skip-permissions "<prompt>"`). These let the agent proceed
/// without interactive approval gates — intended exclusively for `agentflare
/// work`'s autonomous code path, never for `agentflare run --print`.
/// `None` (or empty) means the agent has no known bypass flag (the user
/// must acknowledge via `--timeout` that any hang on missing permission is
/// acceptable, or the run will stall).
#[allow(dead_code)]
#[must_use]
pub fn autonomous_args(agent: Agent) -> Option<&'static [&'static str]> {
    match agent {
        Agent::ClaudeCode => Some(&["--dangerously-skip-permissions"]),
        Agent::Codex => Some(&["--full-auto"]),
        Agent::GeminiCli => Some(&["--yolo"]),
        Agent::Opencode => None,
        _ => None,
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
    fn registry_has_seventeen_cli_tier_and_three_extension_tier() {
        let cli_count = REGISTRY.iter().filter(|s| s.tier == Tier::Cli).count();
        let ext_count = REGISTRY
            .iter()
            .filter(|s| s.tier == Tier::Extension)
            .count();
        assert_eq!(cli_count, 17);
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
        assert_eq!(spec(Agent::Cody).display_name, "cody");
    }

    #[test]
    fn as_str_matches_display_name_for_every_variant() {
        for s in REGISTRY {
            assert_eq!(s.id.as_str(), s.display_name);
        }
    }

    #[test]
    fn headless_args_map_known_print_modes() {
        assert_eq!(headless_args(Agent::ClaudeCode), Some(&["-p"][..]));
        assert_eq!(headless_args(Agent::Codex), Some(&["exec"][..]));
        assert_eq!(headless_args(Agent::GeminiCli), Some(&["-p"][..]));
        assert_eq!(headless_args(Agent::Opencode), Some(&["run"][..]));
    }

    #[test]
    fn headless_args_none_for_agents_without_a_print_mode() {
        // Editor-embedded / unmapped agents have no headless invocation.
        assert_eq!(headless_args(Agent::Cursor), None);
        assert_eq!(headless_args(Agent::VscodeCopilot), None);
    }
}
