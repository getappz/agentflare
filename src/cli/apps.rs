//! `agentflare apps` — run a self-contained AgentFlare App directory
//! end-to-end through the embedded workflow engine, projecting the App's
//! agent-neutral personas/skills/tools into a fresh scratch dir per step
//! (mirrors `cli::workflow`'s pattern, using `app_send_hook` in place of
//! `agent_send_hook`).

use std::path::{Path, PathBuf};

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
                    Ok(run_id) => println!("{run_id}"),
                    Err(e) => {
                        ui::error(&format!("apps run failed: {e}"));
                        std::process::exit(1);
                    }
                }
            }
        }
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
