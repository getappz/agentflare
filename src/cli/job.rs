//! `agentflare job` -- operator controls over dispatched jobs. Thin wrapper
//! over `crate::job_controls`, shared with the MCP `item` tool, the
//! dashboard and the chat commands.

use clap::{Args, Subcommand};

#[derive(Args)]
pub struct JobArgs {
    #[command(subcommand)]
    pub command: JobCommands,
}

#[derive(Subcommand)]
pub enum JobCommands {
    /// Cancel one job: a queued job never starts, a running one has its agent
    /// killed; either way it is not retried
    Cancel {
        /// Job id (see `agentflare daemon` / the dashboard's job list)
        job_id: String,
    },
    /// Pause the item a job is working on (or an item directly): its run
    /// stops at the next step boundary, keeping worktree and run state
    Pause {
        /// Job id or item id (#1 or UUID)
        target: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Resume a paused item (by the job that was paused, or the item)
    Resume {
        /// Job id or item id (#1 or UUID)
        target: String,
    },
}

impl JobArgs {
    pub fn run(self) {
        match self.command {
            JobCommands::Cancel { job_id } => match crate::job_controls::cancel_job(&job_id) {
                Ok(resp) => println!("{resp}"),
                Err(e) => {
                    crate::ui::error(&e);
                    std::process::exit(1);
                }
            },
            JobCommands::Pause { target, reason } => super::item::run_item_control(
                &crate::job_controls::item_ref_for(&target),
                "pause",
                reason,
                None,
            ),
            JobCommands::Resume { target } => super::item::run_item_control(
                &crate::job_controls::item_ref_for(&target),
                "resume",
                None,
                None,
            ),
        }
    }
}
