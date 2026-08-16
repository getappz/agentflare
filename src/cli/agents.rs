use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum AgentsAction {
    /// List every AI coding agent detected on PATH.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Check each detected agent's config wiring for problems.
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Install an agent's CLI onto this machine.
    Install {
        agent: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Update an already-installed agent's CLI to the latest version.
    Update {
        agent: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove an agent's CLI from this machine.
    Uninstall {
        agent: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Start a session with the given agent.
    Launch {
        agent: String,
        #[arg(long)]
        model: Option<String>,
        #[arg(long)]
        mode: Option<String>,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

/// Detect, install, update, and launch AI coding agent CLIs.
#[derive(Args)]
pub struct AgentsArgs {
    #[command(subcommand)]
    pub action: AgentsAction,
}

impl AgentsArgs {
    pub fn run(self) {
        match self.action {
            AgentsAction::List { json } => crate::agents::cli_list(json),
            AgentsAction::Doctor { json } => crate::agents::cli_doctor(json),
            AgentsAction::Install { agent, dry_run } => crate::agents::cli_install(&agent, dry_run),
            AgentsAction::Update { agent, dry_run } => crate::agents::cli_update(&agent, dry_run),
            AgentsAction::Uninstall { agent, dry_run } => {
                crate::agents::cli_uninstall(&agent, dry_run)
            }
            AgentsAction::Launch {
                agent,
                model,
                mode,
                args,
            } => crate::agents::cli_launch(&agent, model.as_deref(), mode.as_deref(), &args),
        }
    }
}
