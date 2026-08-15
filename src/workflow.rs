//! Workflow service: runs JSON-defined agent pipelines through the embedded
//! `flare-workflow` engine with a durable SQLite store, dispatching each
//! step's prompt to a named agent via the headless agent runner.
//!
//! Shared by the `mcp__flare__workflow` tool and the `agentflare workflow`
//! CLI.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::LazyLock;
use std::time::Duration;

use flare_workflow::StateStore;
use flare_workflow::engine::WorkflowEngine;
use flare_workflow::json::{JsonWorkflow, PipelineData, SendMessage, compile_workflow};
use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::types::{EntryResult, WorkflowId, WorkflowRunId, WorkflowStatus};

/// Shared multi-thread runtime for the async engine. Must live for the
/// process lifetime: `start_workflow` spawns execution tasks that outlive the
/// call, so a per-call runtime would be dropped (cancelling in-flight runs).
static WORKFLOW_RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("build workflow runtime")
});

/// The shared engine runtime (see [`WORKFLOW_RT`]).
pub(crate) fn blocking_runtime() -> &'static tokio::runtime::Runtime {
    &WORKFLOW_RT
}

/// A cloned [`tokio::runtime::Handle`] onto [`WORKFLOW_RT`] — for engines
/// (e.g. `work_item_pipeline::engine()`) that need to pin their execution
/// tasks there via `WorkflowEngine::with_runtime_handle` without holding a
/// reference to the `Runtime` itself.
pub(crate) fn blocking_runtime_handle() -> tokio::runtime::Handle {
    WORKFLOW_RT.handle().clone()
}

/// Parse a run id string (v7 UUID).
fn parse_run_id(s: &str) -> Result<WorkflowRunId, String> {
    WorkflowRunId::from_str(s).map_err(|e| format!("invalid run id '{s}': {e}"))
}

/// Default SQLite store location for workflow runs.
pub fn default_db_path() -> PathBuf {
    crate::paths::home()
        .join(".agentflare")
        .join("workflows.db")
}

/// Default hard/idle subprocess timeouts when a step doesn't override them.
const DEFAULT_HARD_CAP_SECS: u64 = 600;
const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;

/// Wrap the headless agent runner as the engine's `SendMessage` hook. Honors
/// a `JsonStep`'s `model`/`args`/`hard_cap_secs`/`idle_timeout_secs`
/// overrides (via `StepInvocation`) so a workflow author can tune all of
/// them as data, falling back to the defaults above when unset.
pub(crate) fn agent_send_hook() -> SendMessage {
    std::sync::Arc::new(|inv: flare_workflow::json::StepInvocation| {
        Box::pin(async move {
            let flare_workflow::json::StepInvocation {
                agent,
                prompt,
                model,
                args,
                hard_cap_secs,
                idle_timeout_secs,
            } = inv;
            // `--model` ahead of any caller-supplied flags, mirroring
            // `run_launch_env`'s existing `--model` placement for the
            // interactive launch path.
            let mut extra_args = Vec::with_capacity(args.len() + 2);
            if let Some(m) = model {
                extra_args.push("--model".to_string());
                extra_args.push(m);
            }
            extra_args.extend(args);
            let hard_cap = Duration::from_secs(hard_cap_secs.unwrap_or(DEFAULT_HARD_CAP_SECS));
            let idle_timeout =
                Duration::from_secs(idle_timeout_secs.unwrap_or(DEFAULT_IDLE_TIMEOUT_SECS));
            // `run_headless` blocks synchronously (subprocess, up to 900s) —
            // it must run inside the async block via `spawn_blocking`, not
            // before it, or the call already happened on whatever thread
            // invoked this closure before the returned future was ever
            // polled/awaited.
            let outcome = tokio::task::spawn_blocking(move || {
                crate::agent_launch::run_headless(
                    agent_registry::REGISTRY,
                    &agent,
                    &prompt,
                    hard_cap,
                    idle_timeout,
                    &extra_args,
                )
            })
            .await
            .map_err(|e| format!("agent task panicked: {e}"))?;
            match outcome {
                crate::agent_launch::HeadlessOutcome::Ok(reply) => {
                    // Agent CLIs don't report token counts; 0 keeps accounting
                    // honest (unknown rather than fabricated).
                    Ok((reply, 0, 0))
                }
                crate::agent_launch::HeadlessOutcome::UnknownAgent(e)
                | crate::agent_launch::HeadlessOutcome::NotHeadless(e)
                | crate::agent_launch::HeadlessOutcome::NotFound(e)
                | crate::agent_launch::HeadlessOutcome::Failed(e) => Err(e),
            }
        })
    })
}

/// Directory a project keeps its custom `flare-workflow` JSON definitions
/// in, sibling to `.agentflare/config.toml` (see `github::bridge::config`).
pub(crate) fn project_workflows_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(".agentflare").join("workflows")
}

/// Resolves a project-local workflow by name: reads
/// `<repo_root>/.agentflare/workflows/<name>.json` as JSONC (comments and
/// trailing commas allowed — see `crate::jsonc`) and re-serializes it to
/// strict JSON for `run_workflow_json_async`, which the MCP `definition`
/// field also feeds. Rejects any `name` that isn't a single path component,
/// so a caller can't escape the workflows directory via `..`/`/`.
pub fn resolve_named_definition(repo_root: &Path, name: &str) -> Result<String, String> {
    if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
        return Err(format!("invalid workflow name: '{name}'"));
    }
    let path = project_workflows_dir(repo_root).join(format!("{name}.json"));
    if !path.is_file() {
        return Err(format!(
            "no workflow named '{name}' in {}",
            project_workflows_dir(repo_root).display()
        ));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let value = crate::jsonc::parse_jsonc(&text)
        .map_err(|e| format!("{}: invalid workflow JSON: {e}", path.display()))?;
    serde_json::to_string(&value).map_err(|e| e.to_string())
}

/// Lists the names (file stems) of every `.json` workflow definition in a
/// project's `.agentflare/workflows/` directory, sorted.
pub fn list_workflow_definitions(repo_root: &Path) -> Vec<String> {
    let dir = project_workflows_dir(repo_root);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .filter_map(|e| {
            e.path()
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    names
}

fn open_store(db_path: &Path) -> Result<SqliteStore<PipelineData>, String> {
    SqliteStore::open_file(db_path).map_err(|e| e.to_string())
}

/// Build an engine that spawns its execution tasks on [`WORKFLOW_RT`],
/// regardless of which runtime is calling into it. Needed because
/// `run_workflow_json_async` (and friends) are `await`ed directly from the
/// MCP handler on the daemon's own runtime, not just `block_on`'d from
/// `WORKFLOW_RT` via the CLI's sync wrapper — without this, `start_workflow`'s
/// internal `tokio::spawn` would land on whichever runtime called in.
fn engine_for(
    store: SqliteStore<PipelineData>,
) -> WorkflowEngine<PipelineData, SqliteStore<PipelineData>> {
    WorkflowEngine::<PipelineData, _>::with_store(store)
        .with_runtime_handle(WORKFLOW_RT.handle().clone())
}

/// Register a JSON workflow and start a run. Returns `{run_id, workflow_id}`.
/// Sync wrapper (CLI, sync contexts) — see [`run_workflow_json_async`].
pub fn run_workflow_json(
    definition_json: &str,
    input: &str,
    db_path: &Path,
) -> Result<(WorkflowRunId, WorkflowId), String> {
    WORKFLOW_RT
        .block_on(run_workflow_json_async(
            definition_json,
            input,
            db_path,
            agent_send_hook(),
        ))
        .map_err(|e| e.to_string())
}

/// Async core used by the MCP handler (already on a runtime); never nested
/// `block_on`.
pub(crate) async fn run_workflow_json_async(
    definition_json: &str,
    input: &str,
    db_path: &Path,
    send: SendMessage,
) -> Result<(WorkflowRunId, WorkflowId), String> {
    run_workflow_json_with_params_async(
        definition_json,
        input,
        serde_json::Value::Null,
        db_path,
        send,
    )
    .await
}

/// Same as [`run_workflow_json_async`] with a structured `params` payload —
/// used by the MCP `workflow` tool's `run` action when a `params` field is
/// supplied.
pub(crate) async fn run_workflow_json_with_params_async(
    definition_json: &str,
    input: &str,
    params: serde_json::Value,
    db_path: &Path,
    send: SendMessage,
) -> Result<(WorkflowRunId, WorkflowId), String> {
    let json: JsonWorkflow =
        serde_json::from_str(definition_json).map_err(|e| format!("invalid workflow JSON: {e}"))?;
    let wf = compile_workflow(&json, send).map_err(|e| e.to_string())?;
    let workflow_id = wf.id.clone();
    let name = wf.name.clone();

    let store = open_store(db_path)?;
    let engine = engine_for(store);
    engine
        .register_workflow(wf)
        .map_err(|e| format!("invalid workflow: {e}"))?;

    let run_id = engine
        .start_workflow_with_params(workflow_id.clone(), PipelineData, input.to_string(), params)
        .await
        .map_err(|e| e.to_string())?;
    eprintln!("agentflare-workflow: run {run_id} started for '{name}'");
    Ok((run_id, workflow_id))
}

/// Same as [`run_workflow_json`] with an injectable `SendMessage` hook — used
/// by tests to drive steps without an installed agent binary.
#[cfg(test)]
pub(crate) fn run_workflow_json_with_sender(
    definition_json: &str,
    input: &str,
    db_path: &Path,
    send: SendMessage,
) -> Result<(WorkflowRunId, WorkflowId), String> {
    WORKFLOW_RT
        .block_on(run_workflow_json_async(
            definition_json,
            input,
            db_path,
            send,
        ))
        .map_err(|e| e.to_string())
}

/// Full status of a run: state, per-step results, journal tail, output/error.
/// Sync wrapper — see [`workflow_status_async`].
pub fn workflow_status(run_id: &str, db_path: &Path) -> Result<serde_json::Value, String> {
    WORKFLOW_RT
        .block_on(workflow_status_async(run_id, db_path))
        .map_err(|e| e.to_string())
}

pub(crate) async fn workflow_status_async(
    run_id: &str,
    db_path: &Path,
) -> Result<serde_json::Value, String> {
    let store = open_store(db_path)?;
    let engine = WorkflowEngine::<PipelineData, _>::with_store(store);
    let run_id = parse_run_id(run_id)?;

    let state = engine.get_status(run_id).await.map_err(|e| e.to_string())?;
    let journal = engine
        .state_store()
        .journal(run_id)
        .await
        .map_err(|e| e.to_string())?;
    let journal_tail: Vec<serde_json::Value> = journal
        .iter()
        .rev()
        .take(20)
        .map(|e| {
            serde_json::json!({
                "entry_type": e.entry_type(),
                "completed": e.is_completed(),
            })
        })
        .collect();

    Ok(serde_json::json!({
        "run_id": state.run_id.to_string(),
        "workflow_id": state.workflow_id.to_string(),
        "status": status_str(&state.status),
        "current_step": state.current_step.map(|s| s.to_string()),
        "input": state.input,
        "output": state.output,
        "error": state.error,
        "steps": state.step_states.iter().map(|(id, ss)| serde_json::json!({
            "step_id": id.to_string(),
            "status": step_status_str(&ss.status),
            "attempt": ss.attempt,
            "last_error": ss.last_error,
            "input_tokens": ss.input_tokens,
            "output_tokens": ss.output_tokens,
            "duration_ms": ss.duration_ms,
        })).collect::<Vec<_>>(),
        "variables": state.variables,
        "journal_tail": journal_tail,
    }))
}

/// List run summaries. Sync wrapper — see [`list_workflows_async`].
pub fn list_workflows(db_path: &Path) -> Result<Vec<serde_json::Value>, String> {
    WORKFLOW_RT
        .block_on(list_workflows_async(db_path))
        .map_err(|e| e.to_string())
}

pub(crate) async fn list_workflows_async(db_path: &Path) -> Result<Vec<serde_json::Value>, String> {
    let store = open_store(db_path)?;
    let engine = WorkflowEngine::<PipelineData, _>::with_store(store);
    let all = engine
        .state_store()
        .list_all()
        .await
        .map_err(|e| e.to_string())?;
    Ok(all
        .into_iter()
        .map(|s| {
            serde_json::json!({
                "run_id": s.run_id.to_string(),
                "workflow_id": s.workflow_id.to_string(),
                "status": status_str(&s.status),
                "created_at": s.created_at.to_rfc3339(),
                "updated_at": s.updated_at.to_rfc3339(),
            })
        })
        .collect())
}

/// Resolve a pending `WaitEvent` on a run. Sync wrapper — see
/// [`complete_workflow_event_async`].
pub fn complete_workflow_event(
    run_id: &str,
    name: &str,
    result: &str,
    db_path: &Path,
) -> Result<(), String> {
    WORKFLOW_RT
        .block_on(complete_workflow_event_async(run_id, name, result, db_path))
        .map_err(|e| e.to_string())
}

pub(crate) async fn complete_workflow_event_async(
    run_id: &str,
    name: &str,
    result: &str,
    db_path: &Path,
) -> Result<(), String> {
    let store = open_store(db_path)?;
    let engine = WorkflowEngine::<PipelineData, _>::with_store(store);
    let run_id = parse_run_id(run_id)?;
    engine
        .complete_event(
            run_id,
            name,
            EntryResult::Success(result.as_bytes().to_vec()),
        )
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn status_str(s: &WorkflowStatus) -> &'static str {
    match s {
        WorkflowStatus::Pending => "pending",
        WorkflowStatus::Running => "running",
        WorkflowStatus::Paused => "paused",
        WorkflowStatus::Completed => "completed",
        WorkflowStatus::Failed => "failed",
        WorkflowStatus::Cancelled => "cancelled",
    }
}

fn step_status_str(s: &flare_workflow::StepStatus) -> &'static str {
    use flare_workflow::StepStatus;
    match s {
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Succeeded => "succeeded",
        StepStatus::Failed => "failed",
        StepStatus::Retrying => "retrying",
        StepStatus::Skipped => "skipped",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn mock_send() -> SendMessage {
        Arc::new(|inv: flare_workflow::json::StepInvocation| {
            Box::pin(async move {
                Ok((
                    format!("[{} processed: {}]", inv.agent, inv.prompt),
                    inv.prompt.len() as u64,
                    0,
                ))
            })
        })
    }

    #[test]
    fn run_status_list_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("wf.db");

        let (run_id, workflow_id) = run_workflow_json_with_sender(
            r#"{
                "name": "pipeline",
                "steps": [
                    {"name": "a", "agent": "opencode", "prompt": "Do: {{input}}", "output_var": "analysis"},
                    {"name": "b", "agent": "opencode", "prompt": "Then: {{analysis}}"}
                ]
            }"#,
            "seed",
            &db,
            mock_send(),
        )
        .unwrap();

        let rt = blocking_runtime();
        let run_id_owned = run_id;
        rt.block_on(async {
            // Let the in-process run finish.
            for _ in 0..100 {
                let store = SqliteStore::<PipelineData>::open_file(&db).unwrap();
                let engine = WorkflowEngine::<PipelineData, _>::with_store(store);
                if let Ok(s) = engine.get_status(run_id_owned).await
                    && s.status == WorkflowStatus::Completed
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        });

        let status = workflow_status(&run_id.to_string(), &db).unwrap();
        assert_eq!(status["status"], "completed");
        assert_eq!(status["workflow_id"], workflow_id.to_string());
        assert_eq!(status["steps"].as_array().unwrap().len(), 2);
        assert!(status["variables"]["analysis"].is_string());
        assert_eq!(status["journal_tail"].as_array().unwrap().len(), 4);

        let runs = list_workflows(&db).unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0]["status"], "completed");
    }

    #[test]
    fn complete_event_resolves_wait_step() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("wf.db");

        let (run_id, _) = run_workflow_json_with_sender(
            r#"{
                "name": "approval",
                "steps": [
                    {"name": "ask", "agent": "opencode", "prompt": "Ask: {{input}}", "mode": {"wait_event": {"name": "approve", "timeout_secs": 10}}},
                    {"name": "after", "agent": "opencode", "prompt": "After: {{input}}"}
                ]
            }"#,
            "needs approval",
            &db,
            mock_send(),
        )
        .unwrap();

        // Wait for the run to arm on the wait event, then complete it.
        let rt = blocking_runtime();
        let mut resolved = false;
        for _ in 0..100 {
            let status = workflow_status(&run_id.to_string(), &db).unwrap();
            let arming = status["journal_tail"]
                .as_array()
                .map(|t| t.iter().any(|e| e["entry_type"] == "wait_event"))
                .unwrap_or(false);
            if arming {
                complete_workflow_event(&run_id.to_string(), "approve", "yes", &db).unwrap();
                resolved = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(resolved, "run never armed on the wait event");

        rt.block_on(async {
            for _ in 0..100 {
                let store = SqliteStore::<PipelineData>::open_file(&db).unwrap();
                let engine = WorkflowEngine::<PipelineData, _>::with_store(store);
                if let Ok(s) = engine.get_status(run_id).await
                    && s.status == WorkflowStatus::Completed
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            panic!("workflow did not complete after event resolution");
        });

        let status = workflow_status(&run_id.to_string(), &db).unwrap();
        assert_eq!(status["status"], "completed");
    }

    #[test]
    fn invalid_json_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("wf.db");
        let err = run_workflow_json_with_sender("{not json", "x", &db, mock_send()).unwrap_err();
        assert!(err.contains("invalid workflow JSON"));
    }

    /// Run `git` in a repo dir, returning trimmed stdout or panicking with stderr.
    fn git(repo: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap_or_else(|e| panic!("git {args:?} failed to spawn: {e}"));
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A real coder->reviewer->PR pipeline: steps perform actual git/file work
    /// in a temp repo — the flow the agentflare jobs/github bridge drives for
    /// a dispatched item (implement on a branch -> review the real diff ->
    /// publish the PR).
    #[test]
    fn coder_reviewer_pr_pipeline_runs_real_git_flow() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("wf.db");
        let repo = dir.path().join("repo");

        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        std::fs::write(repo.join("README.md"), "# demo\n").unwrap();
        git(&repo, &["add", "."]);
        git(
            &repo,
            &[
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@t",
                "commit",
                "-m",
                "initial",
            ],
        );

        let repo_path = repo.clone();
        let send: SendMessage = Arc::new(move |inv: flare_workflow::json::StepInvocation| {
            let agent = inv.agent;
            let repo = repo_path.clone();
            Box::pin(async move {
                let branch = "feature/greet";
                match agent.as_str() {
                    // coder: do the real work — create the branch, write the
                    // feature, commit it.
                    "coder" => {
                        git(&repo, &["checkout", "-b", branch]);
                        std::fs::create_dir_all(repo.join("src")).unwrap();
                        std::fs::write(
                            repo.join("src").join("lib.py"),
                            "def greet(name):\n    return f\"hello {name}\"\n",
                        )
                        .unwrap();
                        git(&repo, &["add", "."]);
                        git(
                            &repo,
                            &[
                                "-c",
                                "user.name=T",
                                "-c",
                                "user.email=t@t",
                                "commit",
                                "-m",
                                "add greet",
                            ],
                        );
                        Ok((
                            "Implemented greet() in src/lib.py on branch feature/greet".to_string(),
                            1,
                            1,
                        ))
                    }
                    // reviewer: review the REAL committed diff; approve only if
                    // the change is actually present.
                    "reviewer" => {
                        let diff = git(&repo, &["diff", "main...HEAD", "--", "src/lib.py"]);
                        if diff.contains("def greet") {
                            Ok((
                                "APPROVED — diff introduces greet() with a docstring".to_string(),
                                1,
                                1,
                            ))
                        } else {
                            Ok(("needs work: no greet() in the diff".to_string(), 1, 1))
                        }
                    }
                    // pr: publish — simulate the github-bridge PR step by
                    // creating a PR ref from the feature branch.
                    "pr" => {
                        let head = git(&repo, &["rev-parse", "HEAD"]);
                        git(&repo, &["update-ref", "refs/heads/pr/simulate", &head]);
                        Ok((
                            "PR opened: feature/greet -> main (simulated PR ref)".to_string(),
                            1,
                            1,
                        ))
                    }
                    other => Err(format!("unknown agent: {other}")),
                }
            })
        });

        let definition = r#"{
            "name": "coder-reviewer-pr",
            "description": "implement a change, review the real diff until approved, open the PR",
            "steps": [
                {"name": "coder", "agent": "coder", "prompt": "Implement this task:\n{{input}}", "output_var": "change"},
                {"name": "reviewer", "agent": "reviewer", "prompt": "Review this change and reply APPROVED when it is correct:\n{{change}}", "mode": {"loop": {"max_iterations": 5, "until": "APPROVED"}}, "output_var": "review"},
                {"name": "open-pr", "agent": "pr", "prompt": "Publish a PR for the approved change:\n{{review}}"}
            ]
        }"#;

        let (run_id, _) = run_workflow_json_with_sender(
            definition,
            "Add a greet() function to src/lib.py with a test",
            &db,
            send,
        )
        .unwrap();

        // Wait for completion.
        blocking_runtime().block_on(async {
            for _ in 0..300 {
                let store = SqliteStore::<PipelineData>::open_file(&db).unwrap();
                let engine = WorkflowEngine::<PipelineData, _>::with_store(store);
                if let Ok(s) = engine.get_status(run_id).await
                    && s.status == WorkflowStatus::Completed
                {
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
            panic!("workflow did not complete in time");
        });

        // The real effects: branch + committed feature + PR ref exist, and the
        // reviewer approved the actual diff (loop terminated after one pass).
        assert_eq!(git(&repo, &["branch", "--show-current"]), "feature/greet");
        let lib = std::fs::read_to_string(repo.join("src").join("lib.py")).unwrap();
        assert!(
            lib.contains("def greet"),
            "coder step must write the real file"
        );
        git(&repo, &["rev-parse", "--verify", "refs/heads/pr/simulate"]);

        let status = workflow_status(&run_id.to_string(), &db).unwrap();
        assert_eq!(status["status"], "completed");
        let steps = status["steps"].as_array().unwrap();
        assert_eq!(steps.len(), 3);
        let review = steps
            .iter()
            .find(|s| s["step_id"] == "reviewer")
            .expect("reviewer step recorded");
        assert_eq!(review["attempt"], 1, "loop approved on first real diff");
        assert!(status["input"].as_str().unwrap().contains("PR opened"));
    }

    #[test]
    fn resolve_named_definition_finds_a_project_workflow() {
        let repo = tempfile::tempdir().unwrap();
        let workflows_dir = project_workflows_dir(repo.path());
        std::fs::create_dir_all(&workflows_dir).unwrap();
        std::fs::write(
            workflows_dir.join("release-check.json"),
            r#"{
                // a project-authored workflow
                "name": "release-check",
                "steps": [
                    {"name": "a", "agent": "opencode", "prompt": "check"},
                ]
            }"#,
        )
        .unwrap();

        let definition = resolve_named_definition(repo.path(), "release-check").unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&definition).unwrap();
        assert_eq!(parsed["name"], "release-check");
    }

    #[test]
    fn resolve_named_definition_rejects_missing_and_traversal_names() {
        let repo = tempfile::tempdir().unwrap();

        assert!(resolve_named_definition(repo.path(), "nope").is_err());
        assert!(resolve_named_definition(repo.path(), "../etc/passwd").is_err());
        assert!(resolve_named_definition(repo.path(), "sub/dir").is_err());
    }

    #[test]
    fn list_workflow_definitions_lists_json_files_sorted() {
        let repo = tempfile::tempdir().unwrap();
        let workflows_dir = project_workflows_dir(repo.path());
        std::fs::create_dir_all(&workflows_dir).unwrap();
        std::fs::write(workflows_dir.join("b.json"), "{}").unwrap();
        std::fs::write(workflows_dir.join("a.json"), "{}").unwrap();
        std::fs::write(workflows_dir.join("notes.txt"), "ignore me").unwrap();

        assert_eq!(
            list_workflow_definitions(repo.path()),
            vec!["a".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn list_workflow_definitions_empty_when_no_directory() {
        let repo = tempfile::tempdir().unwrap();
        assert!(list_workflow_definitions(repo.path()).is_empty());
    }
}
