mod components;
mod coaching;
mod cost;
mod engram_install;
mod hook;
mod init;
mod optimize;
mod paths;
mod pricing;
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
    /// Print today's Claude Code token usage and estimated cost, by model.
    Cost,
    /// Manage local coaching rules surfaced alongside built-in nudges.
    Coaching {
        #[command(subcommand)]
        action: CoachingAction,
    },
}

#[derive(Subcommand)]
enum CoachingAction {
    /// List all active coaching rules.
    List,
    /// Add or update a coaching rule.
    Apply {
        id: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: String,
    },
    /// Remove a coaching rule.
    Remove { id: String },
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
    PreToolUse {
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
            HookEvent::PreToolUse { agent } => hook::pre_tool_use(agent.as_str()),
        },
        Commands::Cost => cost::run(),
        Commands::Coaching { action } => match action {
            CoachingAction::List => coaching::print_list(),
            CoachingAction::Apply { id, title, body } => coaching::cli_apply(&id, &title, &body),
            CoachingAction::Remove { id } => coaching::cli_remove(&id),
        },
    }
}
