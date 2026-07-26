use crate::coaching::rule::RuleTier;
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
        /// Rule tier: override (default) or builtin. Builtin rules are
        /// re-materialized by `agentflare init` so user edits are not lost.
        #[arg(long)]
        tier: Option<RuleTier>,
        /// Host to sync/coach this rule to (comma- or repeatable). Known values:
        /// claude-code, opencode, cursor, codex, windsurf, vscode-copilot, cline.
        #[arg(long, value_delimiter = ',')]
        sync: Vec<String>,
    },
    Remove {
        id: String,
    },
    /// Regenerate every synced rule file for one host, or all of them.
    Sync {
        /// Agent host to sync rules for (default: all known hosts).
        #[arg(long)]
        agent: Option<String>,
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
                tier,
                sync,
            } => crate::coaching::cli_apply(
                &id,
                &title,
                &body,
                trigger_tool,
                trigger_auto,
                tier,
                sync,
            ),
            CoachingAction::Remove { id } => crate::coaching::cli_remove(&id),
            CoachingAction::Sync { agent } => crate::coaching::cli_sync(agent.as_deref()),
        }
    }
}
