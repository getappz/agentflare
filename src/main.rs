mod components;
mod engram_install;
mod hook;
mod init;
mod paths;
mod rule_text;
mod state;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Copy, Clone, ValueEnum, Debug)]
#[value(rename_all = "kebab-case")]
enum Agent {
    ClaudeCode,
    Codex,
    Cursor,
    Windsurf,
    VscodeCopilot,
    Cline,
    Continue,
}

impl Agent {
    fn as_str(self) -> &'static str {
        match self {
            Agent::ClaudeCode => "claude-code",
            Agent::Codex => "codex",
            Agent::Cursor => "cursor",
            Agent::Windsurf => "windsurf",
            Agent::VscodeCopilot => "vscode-copilot",
            Agent::Cline => "cline",
            Agent::Continue => "continue",
        }
    }
}

#[derive(Parser)]
#[command(name = "agentflare", version, about = "Optimize AI CLI agents for cost and performance")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Set up agentflare for one agent: writes rules, installs lean-ctx/engram
    /// (and Ponytail/Caveman on Claude Code), wires the hook config directly
    /// where possible. Running this command is the consent.
    Init {
        #[arg(long, value_enum)]
        agent: Agent,
    },
    /// Hook entry points, invoked by whatever `init` (or the Codex plugin
    /// manifest) wired into the target agent's hook config. Not meant to be
    /// run by hand.
    Hook {
        #[command(subcommand)]
        event: HookEvent,
    },
}

#[derive(Subcommand)]
enum HookEvent {
    SessionStart {
        #[arg(long, value_enum)]
        agent: Agent,
    },
    PromptSubmit {
        #[arg(long, value_enum)]
        agent: Agent,
    },
}

fn main() {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init { agent } => init::run(agent.as_str()),
        Commands::Hook { event } => match event {
            HookEvent::SessionStart { agent } => hook::session_start(agent.as_str()),
            HookEvent::PromptSubmit { agent } => hook::prompt_submit(agent.as_str()),
        },
    }
}
