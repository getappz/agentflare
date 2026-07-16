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
        /// Tool name this rule should fire for (repeatable). Omit both this
        /// and --trigger-auto to keep the rule firing at SessionStart.
        #[arg(long = "trigger-tool")]
        trigger_tool: Vec<String>,
        /// Score this rule's title+body via BM25 against the prompt on
        /// every UserPromptSubmit; fires when relevant, no keyword list
        /// to maintain.
        #[arg(long = "trigger-auto")]
        trigger_auto: bool,
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
            CoachingAction::Apply {
                id,
                title,
                body,
                trigger_tool,
                trigger_auto,
            } => crate::coaching::cli_apply(&id, &title, &body, trigger_tool, trigger_auto),
            CoachingAction::Remove { id } => crate::coaching::cli_remove(&id),
        }
    }
}
