//! `agentflare apps` — run a self-contained AgentFlare App directory
//! end-to-end through the embedded workflow engine, projecting the App's
//! agent-neutral personas/skills/tools into a fresh scratch dir per step
//! (mirrors `cli::workflow`'s pattern, using `app_send_hook` in place of
//! `agent_send_hook`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use clap::{Args, Subcommand};

use crate::ui;

#[derive(Args)]
pub struct AppsArgs {
    #[command(subcommand)]
    pub command: AppsCommand,
}

#[derive(Subcommand)]
pub enum AppsCommand {
    /// Run an AgentFlare App end-to-end from its self-contained directory.
    Run(RunArgs),
}

#[derive(Args)]
pub struct RunArgs {
    /// Path to the App's own directory (contains app.toml, workflow.json, ...).
    pub dir: String,
    /// Initial input for the first step.
    #[arg(long, default_value = "")]
    pub input: String,
    /// SQLite store path (defaults to ~/.agentflare/workflows.db).
    #[arg(long)]
    pub db_path: Option<PathBuf>,
}

impl AppsArgs {
    pub fn run(&self) {
        match &self.command {
            AppsCommand::Run(args) => {
                let db_path = args
                    .db_path
                    .clone()
                    .unwrap_or_else(crate::workflow::default_db_path);
                match run_app(Path::new(&args.dir), &args.input, &db_path) {
                    Ok(run_id) => {
                        println!("{run_id}");
                        // `crate::workflow::run_workflow_json_with_sender` only
                        // registers the run and returns — the step tasks it
                        // spawns keep running on `WORKFLOW_RT`'s worker
                        // threads, but those are killed the instant this
                        // process exits (a `static` runtime's `Drop` never
                        // runs on normal process exit). Without blocking here,
                        // `apps run` would print a run id for a run that never
                        // actually executes a single step. Poll the
                        // already-durable SQLite status instead of holding
                        // any in-process engine handle, so this works
                        // regardless of how the run was started.
                        wait_for_run(&run_id, &db_path);
                    }
                    Err(e) => {
                        ui::error(&format!("apps run failed: {e}"));
                        std::process::exit(1);
                    }
                }
            }
        }
    }
}

/// Blocks until `run_id` leaves `pending`/`running`, printing its final
/// status, or until `max_wait` elapses (printing whatever status was last
/// observed). Never fails the process — a timeout still leaves the run
/// progressing in the background for a later `agentflare workflow status`
/// check.
fn wait_for_run(run_id: &str, db_path: &Path) {
    wait_for_run_with_timeout(run_id, db_path, Duration::from_secs(1800));
}

fn wait_for_run_with_timeout(run_id: &str, db_path: &Path, max_wait: Duration) {
    let deadline = Instant::now() + max_wait;
    loop {
        let status = match crate::workflow::workflow_status(run_id, db_path) {
            Ok(status) => status,
            Err(e) => {
                ui::error(&format!("apps run: could not read status: {e}"));
                return;
            }
        };
        let is_terminal = !matches!(
            status.get("status").and_then(|s| s.as_str()),
            Some("pending" | "running")
        );
        if is_terminal || Instant::now() >= deadline {
            println!(
                "{}",
                serde_json::to_string_pretty(&status).unwrap_or_default()
            );
            return;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Loads the App's manifest (and optional tools manifest) from `app_dir`,
/// then registers and starts its workflow with `app_send_hook` as the
/// dispatch sender so each step runs against a projected persona/skill/tool
/// scratch directory instead of the caller's own `.claude/` setup. Returns
/// the new run's id.
pub(crate) fn run_app(app_dir: &Path, input: &str, db_path: &Path) -> Result<String, String> {
    let manifest = agentflare_apps::load_app_manifest(app_dir)?;
    let tools = agentflare_apps::load_tools_manifest(app_dir)?;
    let workflow_json = std::fs::read_to_string(&manifest.workflow)
        .map_err(|e| format!("could not read {}: {e}", manifest.workflow.display()))?;
    let send = crate::workflow::app_send_hook(app_dir.to_path_buf(), tools);
    let (run_id, _workflow_id) =
        crate::workflow::run_workflow_json_with_sender(&workflow_json, input, db_path, send)?;
    Ok(run_id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_end_to_end_against_a_fixture_app_prints_a_run_id() {
        let app_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            app_dir.path().join("app.toml"),
            r#"name = "fixture-app"
version = "0.1.0"
workflow = "workflow.json""#,
        )
        .unwrap();
        std::fs::write(
            app_dir.path().join("workflow.json"),
            r#"{
                "name": "fixture-workflow",
                "steps": [
                    { "name": "step1", "agent": "fixture-agent", "prompt": "{{input}}" }
                ]
            }"#,
        )
        .unwrap();
        std::fs::create_dir_all(app_dir.path().join("personas")).unwrap();
        std::fs::write(
            app_dir.path().join("personas/fixture-agent.md"),
            "# Fixture",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let run_id = run_app(app_dir.path(), "hello", &db_dir.path().join("apps.db")).unwrap();
        assert!(!run_id.is_empty());
    }

    #[test]
    fn wait_for_run_reaches_a_terminal_status_instead_of_hanging_on_pending() {
        // Regression test: `run_workflow_json_with_sender` only registers and
        // starts a run — its step tasks execute on `WORKFLOW_RT`'s worker
        // threads independently of whatever called it. Without a wait loop,
        // `apps run` used to print a run id for a run stuck at "pending"
        // forever once this process exited. "fixture-agent" isn't a real
        // agent-registry entry, so the single step fails fast
        // (`UnknownAgent`) without needing a real CLI binary on PATH — this
        // only exercises that the wait loop observes and returns the
        // resulting terminal status rather than timing out.
        let app_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            app_dir.path().join("app.toml"),
            r#"name = "fixture-app"
version = "0.1.0"
workflow = "workflow.json""#,
        )
        .unwrap();
        std::fs::write(
            app_dir.path().join("workflow.json"),
            r#"{
                "name": "fixture-workflow-2",
                "steps": [
                    { "name": "step1", "agent": "fixture-agent", "prompt": "{{input}}" }
                ]
            }"#,
        )
        .unwrap();
        std::fs::create_dir_all(app_dir.path().join("personas")).unwrap();
        std::fs::write(
            app_dir.path().join("personas/fixture-agent.md"),
            "# Fixture",
        )
        .unwrap();

        let db_dir = tempfile::tempdir().unwrap();
        let db_path = db_dir.path().join("apps.db");
        let run_id = run_app(app_dir.path(), "hello", &db_path).unwrap();

        wait_for_run_with_timeout(&run_id, &db_path, Duration::from_secs(30));

        let status = crate::workflow::workflow_status(&run_id, &db_path).unwrap();
        let final_status = status.get("status").and_then(|s| s.as_str());
        assert!(
            !matches!(final_status, Some("pending" | "running")),
            "expected a terminal status, got {final_status:?}: {status}"
        );
    }

    #[test]
    fn missing_app_toml_is_a_clear_error() {
        let app_dir = tempfile::tempdir().unwrap();
        let db_dir = tempfile::tempdir().unwrap();
        let err = run_app(app_dir.path(), "hi", &db_dir.path().join("apps.db")).unwrap_err();
        assert!(
            err.contains("app.toml"),
            "error should name the missing file: {err}"
        );
    }
}
