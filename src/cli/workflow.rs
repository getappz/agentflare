//! `agentflare workflow` — run and inspect durable agent pipelines through
//! the embedded flare-workflow engine (mirrors mcp__flare__workflow).

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::ui;

#[derive(Parser)]
pub struct WorkflowArgs {
    #[command(subcommand)]
    pub command: WorkflowCommand,
}

#[derive(Subcommand)]
pub enum WorkflowCommand {
    /// Start a JSON-defined workflow from a file.
    Run(RunArgs),
    /// Show a run's state, per-step results, and journal tail.
    Status(StatusArgs),
    /// Resolve a human-in-the-loop WaitEvent.
    CompleteEvent(CompleteEventArgs),
    /// List run summaries.
    List(ListArgs),
}

#[derive(Parser)]
pub struct RunArgs {
    /// Path to the workflow JSON file (OpenFang schema).
    pub file: String,
    /// Initial input for the first step.
    #[arg(long, default_value = "")]
    pub input: String,
    /// SQLite store path (defaults to ~/.agentflare/workflows.db).
    #[arg(long)]
    pub db_path: Option<PathBuf>,
}

#[derive(Parser)]
pub struct StatusArgs {
    /// Run UUID.
    pub run_id: String,
    /// SQLite store path (defaults to ~/.agentflare/workflows.db).
    #[arg(long)]
    pub db_path: Option<PathBuf>,
}

#[derive(Parser)]
pub struct CompleteEventArgs {
    /// Run UUID.
    pub run_id: String,
    /// WaitEvent name to resolve.
    pub name: String,
    /// Completion payload text.
    #[arg(long, default_value = "approved")]
    pub result: String,
    /// SQLite store path (defaults to ~/.agentflare/workflows.db).
    #[arg(long)]
    pub db_path: Option<PathBuf>,
}

#[derive(Parser)]
pub struct ListArgs {
    /// SQLite store path (defaults to ~/.agentflare/workflows.db).
    #[arg(long)]
    pub db_path: Option<PathBuf>,
}

impl WorkflowArgs {
    pub fn run(&self) {
        let db_path =
            |p: &Option<PathBuf>| p.clone().unwrap_or_else(crate::workflow::default_db_path);
        match &self.command {
            WorkflowCommand::Run(args) => {
                let definition = match std::fs::read_to_string(&args.file) {
                    Ok(s) => s,
                    Err(e) => {
                        ui::error(&format!("could not read {}: {e}", args.file));
                        std::process::exit(1);
                    }
                };
                match crate::workflow::run_workflow_json(
                    &definition,
                    &args.input,
                    &db_path(&args.db_path),
                ) {
                    Ok((run_id, _)) => {
                        println!("{run_id}");
                    }
                    Err(e) => {
                        ui::error(&format!("workflow run failed: {e}"));
                        std::process::exit(1);
                    }
                }
            }
            WorkflowCommand::Status(args) => {
                match crate::workflow::workflow_status(&args.run_id, &db_path(&args.db_path)) {
                    Ok(status) => println!(
                        "{}",
                        serde_json::to_string_pretty(&status).unwrap_or_default()
                    ),
                    Err(e) => {
                        ui::error(&e);
                        std::process::exit(1);
                    }
                }
            }
            WorkflowCommand::CompleteEvent(args) => {
                match crate::workflow::complete_workflow_event(
                    &args.run_id,
                    &args.name,
                    &args.result,
                    &db_path(&args.db_path),
                ) {
                    Ok(()) => println!("event '{}' completed", args.name),
                    Err(e) => {
                        ui::error(&e);
                        std::process::exit(1);
                    }
                }
            }
            WorkflowCommand::List(args) => {
                match crate::workflow::list_workflows(&db_path(&args.db_path)) {
                    Ok(runs) => {
                        for run in runs {
                            println!("{}", serde_json::to_string(&run).unwrap_or_default());
                        }
                    }
                    Err(e) => {
                        ui::error(&e);
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}
