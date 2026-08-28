//! `agentflare workflow` — run and inspect durable agent pipelines through
//! the embedded flare-workflow engine (mirrors mcp__flare__workflow).

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};

use crate::ui;

#[derive(Args)]
pub struct WorkflowArgs {
    #[command(subcommand)]
    pub command: WorkflowCommand,
}

#[derive(Subcommand)]
pub enum WorkflowCommand {
    /// Start a JSON-defined workflow from a file, or by name from this
    /// project's `.agentflare/workflows/` directory.
    Run(RunArgs),
    /// Show a run's state, per-step results, and journal tail.
    Status(StatusArgs),
    /// Resolve a human-in-the-loop WaitEvent.
    CompleteEvent(CompleteEventArgs),
    /// Cancel a running/waiting workflow. Already-succeeded steps are left
    /// uncompensated.
    Cancel(CancelArgs),
    /// List run summaries.
    List(ListArgs),
    /// List this project's `.agentflare/workflows/*.json` definitions.
    ListDefinitions,
    /// Aggregate instance/step metrics across runs — counts by status,
    /// average duration, token totals, per-step breakdown.
    Metrics(MetricsArgs),
}

#[derive(Args)]
pub struct RunArgs {
    /// Path to a workflow JSON file, or the name of a workflow committed to
    /// this project's `.agentflare/workflows/<name>.json` (tried when
    /// `file` doesn't exist as a path).
    pub file: String,
    /// Initial input for the first step.
    #[arg(long, default_value = "")]
    pub input: String,
    /// SQLite store path (defaults to ~/.agentflare/workflows.db).
    #[arg(long)]
    pub db_path: Option<PathBuf>,
}

#[derive(Args)]
pub struct StatusArgs {
    /// Run UUID.
    pub run_id: String,
    /// SQLite store path (defaults to ~/.agentflare/workflows.db).
    #[arg(long)]
    pub db_path: Option<PathBuf>,
}

#[derive(Args)]
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

#[derive(Args)]
pub struct CancelArgs {
    /// Run UUID.
    pub run_id: String,
    /// SQLite store path (defaults to ~/.agentflare/workflows.db).
    #[arg(long)]
    pub db_path: Option<PathBuf>,
}

#[derive(Args)]
pub struct ListArgs {
    /// SQLite store path (defaults to ~/.agentflare/workflows.db).
    #[arg(long)]
    pub db_path: Option<PathBuf>,
}

#[derive(Args)]
pub struct MetricsArgs {
    /// Only runs of this workflow definition.
    #[arg(long)]
    pub workflow_id: Option<String>,
    /// Only runs in this status (pending|running|paused|completed|failed|cancelled).
    #[arg(long)]
    pub status: Option<String>,
    /// Only runs created at or after this RFC 3339 timestamp.
    #[arg(long)]
    pub since: Option<String>,
    /// SQLite store path (defaults to ~/.agentflare/workflows.db).
    #[arg(long)]
    pub db_path: Option<PathBuf>,
}

/// Repo root for `.agentflare/workflows/` name resolution — mirrors
/// `cli::github_bridge`'s `repo_root_or_exit`.
fn repo_root_or_exit() -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    match flare_git_core::branch::repo_toplevel(&cwd) {
        Some(root) => root,
        None => {
            ui::error("not inside a git repository");
            std::process::exit(1);
        }
    }
}

impl WorkflowArgs {
    pub fn run(&self) {
        let db_path =
            |p: &Option<PathBuf>| p.clone().unwrap_or_else(crate::workflow::default_db_path);
        match &self.command {
            WorkflowCommand::Run(args) => {
                let definition = if Path::new(&args.file).is_file() {
                    match std::fs::read_to_string(&args.file) {
                        Ok(s) => s,
                        Err(e) => {
                            ui::error(&format!("could not read {}: {e}", args.file));
                            std::process::exit(1);
                        }
                    }
                } else {
                    match crate::workflow::resolve_named_definition(
                        &repo_root_or_exit(),
                        &args.file,
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            ui::error(&e);
                            std::process::exit(1);
                        }
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
            WorkflowCommand::Cancel(args) => {
                match crate::workflow::cancel_workflow(&args.run_id, &db_path(&args.db_path)) {
                    Ok(()) => println!("run '{}' cancelled", args.run_id),
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
            WorkflowCommand::ListDefinitions => {
                for name in crate::workflow::list_workflow_definitions(&repo_root_or_exit()) {
                    println!("{name}");
                }
            }
            WorkflowCommand::Metrics(args) => {
                match crate::workflow::workflow_metrics(
                    args.workflow_id.as_deref(),
                    args.status.as_deref(),
                    args.since.as_deref(),
                    &db_path(&args.db_path),
                ) {
                    Ok(metrics) => println!(
                        "{}",
                        serde_json::to_string_pretty(&metrics).unwrap_or_default()
                    ),
                    Err(e) => {
                        ui::error(&e);
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}
