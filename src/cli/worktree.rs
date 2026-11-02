//! `agentflare worktree` — worktree-local process helpers.

use clap::{Args, Subcommand};
use flare_git_core::worktree::run_tether;
use std::ffi::OsStr;
use std::path::PathBuf;

#[derive(Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    pub command: WorktreeCommand,
}

#[derive(Subcommand)]
pub enum WorktreeCommand {
    /// Run a command in a worktree and kill its process tree when the worktree
    /// directory is removed (item #810).
    Tether(TetherArgs),
}

#[derive(Args)]
pub struct TetherArgs {
    /// Worktree root to watch (defaults to the current directory).
    #[arg(long, value_name = "PATH")]
    pub root: Option<PathBuf>,
    /// Command and arguments to run (place `--` before flags if needed).
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    pub cmd: Vec<String>,
}

impl WorktreeArgs {
    pub fn run(self) {
        match self.command {
            WorktreeCommand::Tether(args) => args.run(),
        }
    }
}

impl TetherArgs {
    pub fn run(self) {
        if self.cmd.is_empty() {
            crate::ui::error("tether: missing command — usage: agentflare worktree tether -- CMD...");
            crate::decide::exit(1);
        }
        let root = match self.root {
            Some(p) => p,
            None => match std::env::current_dir() {
                Ok(p) => p,
                Err(e) => {
                    crate::ui::error(&format!("tether: current directory unavailable: {e}"));
                    crate::decide::exit(1);
                }
            },
        };
        let program = OsStr::new(&self.cmd[0]);
        let args: Vec<&OsStr> = self.cmd[1..].iter().map(OsStr::new).collect();
        match run_tether(&root, program, &args) {
            Ok(code) => crate::decide::exit(code),
            Err(e) => {
                crate::ui::error(&e);
                crate::decide::exit(1);
            }
        }
    }
}
