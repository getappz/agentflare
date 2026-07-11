use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum AgentsAction {
    List {
        #[arg(long)]
        json: bool,
    },
    Doctor {
        #[arg(long)]
        json: bool,
    },
    Install {
        agent: String,
        #[arg(long)]
        dry_run: bool,
    },
    Update {
        agent: String,
        #[arg(long)]
        dry_run: bool,
    },
    Uninstall {
        agent: String,
        #[arg(long)]
        dry_run: bool,
    },
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
