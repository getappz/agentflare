use clap::{Args, Subcommand};

#[derive(Subcommand)]
pub enum CoachingAction {
    List,
    Apply {
        id: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: String,
    },
    Remove {
        id: String,
    },
}

#[derive(Args)]
pub struct CoachingArgs {
    #[command(subcommand)]
    pub action: CoachingAction,
}

impl CoachingArgs {
    pub fn run(self) {
        match self.action {
            CoachingAction::List => crate::coaching::print_list(),
            CoachingAction::Apply { id, title, body } => {
                crate::coaching::cli_apply(&id, &title, &body)
            }
            CoachingAction::Remove { id } => crate::coaching::cli_remove(&id),
        }
    }
}
