//! `agentflare workflow schedule` — administer interval schedules.
//! `fire` writes a queued occurrence and returns. The daemon executes it.

use clap::{Args, Subcommand};

use crate::schedule::admin::{self, NewSchedule, SchedulePatch, ScheduleView};
use crate::ui;

#[derive(Args)]
pub struct ScheduleArgs {
    #[command(subcommand)]
    pub command: ScheduleCommand,
}

#[derive(Subcommand)]
pub enum ScheduleCommand {
    /// Create an interval schedule for a project workflow.
    Create(CreateArgs),
    /// List schedules for this repository (`--all` is explicit).
    List(ListArgs),
    /// Show one schedule by name or id.
    Show(TargetArgs),
    /// Change workflow, interval, params, grace, or overlap. Clears a frozen error.
    Update(UpdateArgs),
    /// Enable scheduled admissions. A disabled row's next slot becomes now + every.
    Enable(TargetArgs),
    /// Stop new scheduled admissions. Does not cancel claimed work.
    Disable(TargetArgs),
    /// Delete the schedule and its occurrence history. Does not cancel a claimed run.
    Remove(TargetArgs),
    /// Enqueue one manual run. Prints the occurrence id; the daemon executes it.
    Fire(FireArgs),
    /// List occurrences, newest first.
    Runs(RunsArgs),
}

#[derive(Args)]
pub struct CreateArgs {
    /// Schedule name (`^[A-Za-z0-9._-]{1,64}$`), unique in this repo.
    pub name: String,
    /// Workflow file stem in `.agentflare/workflows/`.
    #[arg(long)]
    pub workflow: String,
    /// Interval (`30s`, `5m`, `2h`, `1d`, `1w`, `3M`). `m` is minutes, `M` is
    /// 30 days. At least 60 seconds.
    #[arg(long)]
    pub every: String,
    /// JSON object passed as workflow params. `schedule` is reserved.
    #[arg(long)]
    pub params: Option<String>,
    /// How late a slot may still fire (`0s` allows only the exact slot).
    #[arg(long)]
    pub grace: Option<String>,
    /// `skip` (default) or `allow`.
    #[arg(long, default_value = "skip")]
    pub overlap: String,
}

#[derive(Args)]
pub struct ListArgs {
    /// Include schedules from every repository.
    #[arg(long)]
    pub all: bool,
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct TargetArgs {
    /// Schedule name in this repo, or a schedule id.
    pub name: String,
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct UpdateArgs {
    /// Schedule name in this repo, or a schedule id.
    pub name: String,
    /// Workflow file stem in `.agentflare/workflows/`.
    #[arg(long)]
    pub workflow: Option<String>,
    /// New interval. Resets the next slot to now + every.
    #[arg(long)]
    pub every: Option<String>,
    /// JSON object passed as workflow params. `schedule` is reserved.
    #[arg(long)]
    pub params: Option<String>,
    /// How late a slot may still fire.
    #[arg(long)]
    pub grace: Option<String>,
    /// `skip` or `allow`.
    #[arg(long)]
    pub overlap: Option<String>,
}

#[derive(Args)]
pub struct FireArgs {
    /// Schedule name in this repo, or a schedule id.
    pub name: String,
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct RunsArgs {
    /// Schedule name in this repo, or a schedule id.
    pub name: String,
    /// Newest N occurrences. Omit for the full history.
    #[arg(long)]
    pub limit: Option<u32>,
    /// Machine-readable output.
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: &ScheduleArgs) {
    if let Err(e) = dispatch(args) {
        ui::error(&e);
        std::process::exit(1);
    }
}

fn block<T>(fut: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("runtime: {e}"))?
        .block_on(fut)
}

fn dispatch(args: &ScheduleArgs) -> Result<(), String> {
    let db = crate::workflow::default_db_path();
    let now = db_kit::ids::now();
    match &args.command {
        ScheduleCommand::Create(args) => {
            let row = block(admin::create(
                &db,
                NewSchedule {
                    name: args.name.clone(),
                    repo_root: repo_root()?,
                    workflow_name: args.workflow.clone(),
                    params: admin::parse_params(args.params.as_deref())?,
                    every_secs: admin::parse_interval(&args.every)?,
                    grace_secs: args.grace.as_deref().map(admin::parse_grace).transpose()?,
                    overlap: args.overlap.clone(),
                },
                now,
            ))?;
            println!("{}", row.id);
            println!(
                "schedule '{}' every {}s next_fire_at {}",
                row.name, row.every_secs, row.next_fire_at
            );
        }
        ScheduleCommand::List(args) => {
            let repo = if args.all { None } else { Some(repo_root()?) };
            let rows = block(admin::list(&db, repo.as_deref()))?;
            if args.json {
                print_json(&rows);
            } else {
                for row in &rows {
                    println!(
                        "{}\t{}\t{}\tenabled={}\tevery={}s\tnext={}",
                        row.id,
                        row.name,
                        row.workflow_name,
                        u8::from(row.enabled),
                        row.every_secs,
                        row.next_fire_at
                    );
                }
            }
        }
        ScheduleCommand::Show(args) => {
            let row = block(admin::show(&db, Some(&repo_root()?), &args.name))?;
            if args.json {
                print_json(&row);
            } else {
                print_human(&row);
            }
        }
        ScheduleCommand::Update(args) => {
            let row = block(admin::update(
                &db,
                Some(&repo_root()?),
                &args.name,
                SchedulePatch {
                    workflow_name: args.workflow.clone(),
                    params: admin::parse_params(args.params.as_deref())?,
                    every_secs: args
                        .every
                        .as_deref()
                        .map(admin::parse_interval)
                        .transpose()?,
                    grace_secs: args.grace.as_deref().map(admin::parse_grace).transpose()?,
                    overlap: args.overlap.clone(),
                },
                now,
            ))?;
            print_human(&row);
        }
        ScheduleCommand::Enable(args) => {
            let row = block(admin::set_enabled(
                &db,
                Some(&repo_root()?),
                &args.name,
                true,
                now,
            ))?;
            if args.json {
                print_json(&row);
            } else {
                println!("{} enabled", row.id);
            }
        }
        ScheduleCommand::Disable(args) => {
            let row = block(admin::set_enabled(
                &db,
                Some(&repo_root()?),
                &args.name,
                false,
                now,
            ))?;
            if args.json {
                print_json(&row);
            } else {
                println!("{} disabled", row.id);
            }
        }
        ScheduleCommand::Remove(args) => {
            block(admin::remove(&db, Some(&repo_root()?), &args.name))?;
            if args.json {
                println!(r#"{{"status":"removed"}}"#);
            } else {
                println!("removed");
            }
        }
        ScheduleCommand::Fire(args) => {
            let receipt = block(admin::enqueue_manual(
                &db,
                Some(&repo_root()?),
                &args.name,
                now,
            ))?;
            if args.json {
                print_json(&receipt);
            } else {
                println!("{}", receipt.occurrence_id);
                println!("queued; waiting for daemon");
            }
        }
        ScheduleCommand::Runs(args) => {
            let repo = optional_repo()?;
            let limit = args.limit.map(i64::from);
            let rows = block(admin::runs(&db, repo.as_deref(), &args.name, limit))?;
            if args.json {
                print_json(&rows);
            } else {
                for row in &rows {
                    println!(
                        "{}\t{}\t{}\t{}\t{}",
                        row.id,
                        row.trigger,
                        row.status,
                        row.run_id.as_deref().unwrap_or("-"),
                        row.created_at
                    );
                }
            }
        }
    }
    Ok(())
}

fn optional_repo() -> Result<Option<String>, String> {
    match repo_root() {
        Ok(root) => Ok(Some(root)),
        Err(e) if e == "not inside a git repository" => Ok(None),
        Err(e) => Err(e),
    }
}

fn repo_root() -> Result<String, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let root = flare_git_core::branch::repo_toplevel(&cwd)
        .ok_or_else(|| "not inside a git repository".to_string())?;
    let root = if flare_git_core::branch::is_linked_worktree(&root) {
        flare_git_core::branch::main_worktree_root(&root).unwrap_or(root)
    } else {
        root
    };
    Ok(root.to_string_lossy().into_owned())
}

fn print_json(value: &impl serde::Serialize) {
    println!(
        "{}",
        serde_json::to_string_pretty(value).unwrap_or_default()
    );
}

fn print_human(row: &ScheduleView) {
    println!("id: {}", row.id);
    println!("name: {}", row.name);
    println!("workflow: {}", row.workflow_name);
    println!("every_secs: {}", row.every_secs);
    println!(
        "grace_secs: {}",
        row.grace_secs
            .map(|g| g.to_string())
            .unwrap_or_else(|| "-".into())
    );
    println!("effective_grace_secs: {}", row.effective_grace_secs);
    println!("overlap: {}", row.overlap);
    println!("enabled: {}", u8::from(row.enabled));
    println!("next_fire_at: {}", row.next_fire_at);
    println!(
        "last_fire_at: {}",
        row.last_fire_at
            .map(|t| t.to_string())
            .unwrap_or_else(|| "-".into())
    );
    println!(
        "frozen_error: {}",
        row.frozen_error.as_deref().unwrap_or("-")
    );
    println!("repo_root: {}", row.repo_root);
}
