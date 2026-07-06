mod agent_registry;
mod components;
mod coaching;
mod cost;
mod engram_install;
mod hook;
mod init;
mod mcp_server;
mod optimize;
mod paths;
mod pricing;
mod rule_text;
mod state;

use agent_registry::Agent;
use clap::{Parser, Subcommand};

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
    /// Print Claude Code token usage and estimated cost, by model or by project.
    Cost {
        /// Widen the window from today to the last N days (inclusive of today). Omit for today only.
        #[arg(long)]
        days: Option<u32>,
        /// Group totals by project instead of by model.
        #[arg(long)]
        by_project: bool,
    },
    /// Manage local coaching rules surfaced alongside built-in nudges.
    Coaching {
        #[command(subcommand)]
        action: CoachingAction,
    },
    /// Start an MCP (Model Context Protocol) server on stdio,
    /// exposing agentflare optimization state as resources and tools.
    Mcp,
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
        Commands::Cost { days, by_project } => cost::run(days, by_project),
        Commands::Coaching { action } => match action {
            CoachingAction::List => coaching::print_list(),
            CoachingAction::Apply { id, title, body } => coaching::cli_apply(&id, &title, &body),
            CoachingAction::Remove { id } => coaching::cli_remove(&id),
        },
        Commands::Mcp => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to build tokio runtime for mcp server");
            if let Err(e) = runtime.block_on(mcp_server::run()) {
                eprintln!("agentflare mcp: {e}");
                std::process::exit(1);
            }
        }
    }
}
