//! `workflow` MCP tool handler — one-file-per-tool convention (mirrors
//! item.rs/claim.rs/comment.rs). Thin wrapper over `crate::workflow` service:
//! run/status/complete_event/list for durable agent pipelines.

use std::path::{Path, PathBuf};

use super::*;

impl AgentflareMcp {
    pub(super) async fn workflow_impl(&self, req: WorkflowRequest) -> Result<String, ErrorData> {
        // `db_path` is a test-only override so fixtures can use isolated
        // tempdirs (see the tests below); an MCP caller-supplied path is
        // never honored in production, since that would let any client
        // point the workflow store at an arbitrary file on disk.
        let db_path = resolve_workflow_db_path(req.db_path.as_deref(), !cfg!(test));

        match req.action.as_str() {
            "run" => {
                let definition = match (req.definition, req.workflow_name) {
                    (Some(d), _) => d,
                    (None, Some(name)) => {
                        crate::workflow::resolve_named_definition(&self.worktree_repo_root(), &name)
                            .map_err(|e| ErrorData::invalid_params(e, None))?
                    }
                    (None, None) => {
                        return Err(ErrorData::invalid_params(
                            "one of definition or workflow_name is required",
                            None,
                        ));
                    }
                };
                let input = req.input.unwrap_or_default();
                let params = match req.params.as_deref().filter(|s| !s.is_empty()) {
                    Some(text) => serde_json::from_str(text).map_err(|e| {
                        ErrorData::invalid_params(format!("invalid params JSON: {e}"), None)
                    })?,
                    None => serde_json::Value::Null,
                };
                let (run_id, workflow_id) = crate::workflow::run_workflow_json_with_params_async(
                    &definition,
                    &input,
                    params,
                    &db_path,
                    crate::workflow::agent_send_hook(),
                )
                .await
                .map_err(|e| ErrorData::internal_error(e, None))?;
                Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "run_id": run_id.to_string(),
                    "workflow_id": workflow_id.to_string(),
                }))
                .unwrap_or_default())
            }
            "status" => {
                let run_id = req
                    .run_id
                    .ok_or_else(|| ErrorData::invalid_params("run_id is required", None))?;
                let status = crate::workflow::workflow_status_async(&run_id, &db_path)
                    .await
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                Ok(serde_json::to_string_pretty(&status).unwrap_or_default())
            }
            "complete_event" => {
                let run_id = req
                    .run_id
                    .ok_or_else(|| ErrorData::invalid_params("run_id is required", None))?;
                let name = req
                    .name
                    .ok_or_else(|| ErrorData::invalid_params("name is required", None))?;
                let result = req.result.unwrap_or_default();
                crate::workflow::complete_workflow_event_async(&run_id, &name, &result, &db_path)
                    .await
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                Ok(r#"{"status":"completed"}"#.to_string())
            }
            "cancel" => {
                let run_id = req
                    .run_id
                    .ok_or_else(|| ErrorData::invalid_params("run_id is required", None))?;
                crate::workflow::cancel_workflow_async(&run_id, &db_path)
                    .await
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                Ok(r#"{"status":"cancelled"}"#.to_string())
            }
            "list" => {
                let runs = crate::workflow::list_workflows_async(&db_path)
                    .await
                    .map_err(|e| ErrorData::internal_error(e, None))?;
                Ok(serde_json::to_string_pretty(&runs).unwrap_or_default())
            }
            "list_definitions" => {
                let names = crate::workflow::list_workflow_definitions(&self.worktree_repo_root());
                Ok(serde_json::to_string_pretty(&names).unwrap_or_default())
            }
            "metrics" => {
                let metrics = crate::workflow::workflow_metrics_async(
                    req.workflow_id.as_deref(),
                    req.status.as_deref(),
                    req.since.as_deref(),
                    &db_path,
                )
                .await
                .map_err(|e| ErrorData::invalid_params(e, None))?;
                Ok(serde_json::to_string_pretty(&metrics).unwrap_or_default())
            }
            action if action.starts_with("schedule_") => self.schedule_impl(&req, &db_path).await,
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    }

    /// Schedule actions share the workflow tool. `schedule_fire` only enqueues.
    async fn schedule_impl(&self, req: &WorkflowRequest, db: &Path) -> Result<String, ErrorData> {
        let invalid = |e: String| ErrorData::invalid_params(e, None);
        let now = db_kit::ids::now();
        let repo = || -> Result<String, ErrorData> {
            let root = schedule_repo_root(&self.worktree_repo_root()).map_err(invalid)?;
            crate::schedule::canonical_repo_root(&root.to_string_lossy()).map_err(invalid)
        };
        match req.action.as_str() {
            "schedule_create" => {
                let name = req
                    .schedule_name
                    .as_deref()
                    .or(req.schedule.as_deref())
                    .ok_or_else(|| invalid("schedule_name is required".into()))?;
                let workflow_name = req
                    .workflow_name
                    .as_deref()
                    .ok_or_else(|| invalid("workflow_name is required".into()))?;
                let every = req
                    .every
                    .as_deref()
                    .ok_or_else(|| invalid("every is required".into()))?;
                let row = crate::schedule::admin::create(
                    db,
                    crate::schedule::admin::NewSchedule {
                        name: name.to_string(),
                        repo_root: repo()?,
                        workflow_name: workflow_name.to_string(),
                        params: crate::schedule::admin::parse_params(req.params.as_deref())
                            .map_err(invalid)?,
                        every_secs: crate::schedule::admin::parse_interval(every)
                            .map_err(invalid)?,
                        grace_secs: req
                            .grace
                            .as_deref()
                            .map(crate::schedule::admin::parse_grace)
                            .transpose()
                            .map_err(invalid)?,
                        overlap: req.overlap.clone().unwrap_or_else(|| "skip".into()),
                    },
                    now,
                )
                .await
                .map_err(invalid)?;
                Ok(to_json(&row))
            }
            "schedule_list" => {
                let root = if req.all { None } else { Some(repo()?) };
                let rows = crate::schedule::admin::list(db, root.as_deref())
                    .await
                    .map_err(invalid)?;
                Ok(to_json(&rows))
            }
            "schedule_show" => {
                let token = schedule_token(req)?;
                let row = crate::schedule::admin::show(db, Some(&repo()?), token)
                    .await
                    .map_err(invalid)?;
                Ok(to_json(&row))
            }
            "schedule_update" => {
                let token = schedule_token(req)?;
                let row = crate::schedule::admin::update(
                    db,
                    Some(&repo()?),
                    token,
                    crate::schedule::admin::SchedulePatch {
                        workflow_name: req.workflow_name.clone(),
                        params: crate::schedule::admin::parse_params(req.params.as_deref())
                            .map_err(invalid)?,
                        every_secs: req
                            .every
                            .as_deref()
                            .map(crate::schedule::admin::parse_interval)
                            .transpose()
                            .map_err(invalid)?,
                        grace_secs: req
                            .grace
                            .as_deref()
                            .map(crate::schedule::admin::parse_grace)
                            .transpose()
                            .map_err(invalid)?,
                        overlap: req.overlap.clone(),
                    },
                    now,
                )
                .await
                .map_err(invalid)?;
                Ok(to_json(&row))
            }
            "schedule_enable" | "schedule_disable" => {
                let token = schedule_token(req)?;
                let enabled = req.action == "schedule_enable";
                let row =
                    crate::schedule::admin::set_enabled(db, Some(&repo()?), token, enabled, now)
                        .await
                        .map_err(invalid)?;
                Ok(to_json(&row))
            }
            "schedule_remove" => {
                let token = schedule_token(req)?;
                crate::schedule::admin::remove(db, Some(&repo()?), token)
                    .await
                    .map_err(invalid)?;
                Ok(r#"{"status":"removed"}"#.to_string())
            }
            "schedule_fire" => {
                let token = schedule_token(req)?;
                let receipt =
                    crate::schedule::admin::enqueue_manual(db, Some(&repo()?), token, now)
                        .await
                        .map_err(invalid)?;
                Ok(to_json(&receipt))
            }
            "schedule_runs" => {
                let token = schedule_token(req)?;
                let limit = match req.limit {
                    Some(n) => {
                        Some(i64::try_from(n).map_err(|_| invalid("limit must be >= 0".into()))?)
                    }
                    None => None,
                };
                let rows = crate::schedule::admin::runs(db, Some(&repo()?), token, limit)
                    .await
                    .map_err(invalid)?;
                Ok(to_json(&rows))
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    }
}

/// Production ignores a caller-supplied `db_path`. Tests pass `production: false`.
pub(crate) fn resolve_workflow_db_path(requested: Option<&str>, production: bool) -> PathBuf {
    if !production && let Some(path) = requested.map(str::trim).filter(|s| !s.is_empty()) {
        return PathBuf::from(path);
    }
    crate::workflow::default_db_path()
}

/// `worktree_repo_root` returns `worktree_repo_root_override` unchanged.
/// Schedule ops must map a linked-worktree override to the main checkout
/// before `canonical_repo_root`, matching the CLI's repository scope.
fn schedule_repo_root(root: &Path) -> Result<PathBuf, String> {
    if flare_git_core::branch::is_linked_worktree(root) {
        flare_git_core::branch::main_worktree_root(root).ok_or_else(|| {
            format!(
                "cannot resolve a main checkout for linked worktree '{}'",
                root.display()
            )
        })
    } else {
        Ok(root.to_path_buf())
    }
}

fn schedule_token(req: &WorkflowRequest) -> Result<&str, ErrorData> {
    req.schedule
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ErrorData::invalid_params("schedule is required", None))
}

fn to_json(value: &impl serde::Serialize) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::Arc;

    fn mcp() -> AgentflareMcp {
        AgentflareMcp {
            ..Default::default()
        }
    }

    fn req(action: &str, db: &Path) -> WorkflowRequest {
        WorkflowRequest {
            action: action.into(),
            db_path: Some(db.to_string_lossy().to_string()),
            ..Default::default()
        }
    }

    fn mock_send() -> flare_workflow::json::SendMessage {
        Arc::new(|inv: flare_workflow::json::StepInvocation| {
            Box::pin(async move { Ok((format!("[{}] {}", inv.agent, inv.prompt), 1, 1)) })
        })
    }

    #[tokio::test]
    async fn run_rejects_invalid_json() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");
        let mut r = req("run", &db);
        r.definition = Some("{bad json".into());
        let err = mcp().workflow(Parameters(r)).await.unwrap_err();
        assert!(format!("{err}").contains("invalid workflow JSON"));
    }

    #[tokio::test]
    async fn unknown_action_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");
        let err = mcp()
            .workflow(Parameters(req("bogus", &db)))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("unknown action"));
    }

    #[tokio::test]
    async fn run_rejects_invalid_params_json() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");
        let mut r = req("run", &db);
        r.definition = Some(r#"{"name": "wf", "steps": [{"name": "a", "agent": "x"}]}"#.into());
        r.params = Some("{bad json".into());
        let err = mcp().workflow(Parameters(r)).await.unwrap_err();
        assert!(format!("{err}").contains("invalid params JSON"));
    }

    /// The `run_workflow_json_with_params_async` service call the `run`
    /// action's `params` field is parsed into (with an injectable sender, as
    /// `list_and_status_roundtrip_a_run` does for `input`) actually expands
    /// `{{params.x}}` into the prompt.
    #[tokio::test]
    async fn run_with_params_expands_into_prompt() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");

        let (run_id, _) = crate::workflow::run_workflow_json_with_params_async(
            r#"{
                "name": "params-pipeline",
                "steps": [
                    {"name": "greet", "agent": "opencode", "prompt": "Hello {{params.userId}}"}
                ]
            }"#,
            "seed",
            serde_json::json!({"userId": "u-7"}),
            &db,
            mock_send(),
        )
        .await
        .unwrap();

        let mut v = serde_json::Value::Null;
        for _ in 0..100 {
            let mut status_req = req("status", &db);
            status_req.run_id = Some(run_id.to_string());
            let status = mcp().workflow(Parameters(status_req)).await.unwrap();
            v = serde_json::from_str(&status).unwrap();
            if v["status"] == "completed" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(v["status"], "completed");
        assert!(v["input"].as_str().unwrap().contains("Hello u-7"));
    }

    #[tokio::test]
    async fn cancel_action_stops_a_run() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");

        let (run_id, _) = crate::workflow::run_workflow_json_async(
            r#"{
                "name": "cancel-me",
                "steps": [
                    {"name": "ask", "agent": "opencode", "prompt": "Ask: {{input}}", "mode": {"wait_event": {"name": "approve", "timeout_secs": 10}}}
                ]
            }"#,
            "seed",
            &db,
            mock_send(),
        )
        .await
        .unwrap();

        let status_req = || {
            let mut r = req("status", &db);
            r.run_id = Some(run_id.to_string());
            r
        };
        let mut armed = false;
        for _ in 0..100 {
            let status = mcp().workflow(Parameters(status_req())).await.unwrap();
            let v: serde_json::Value = serde_json::from_str(&status).unwrap();
            armed = v["journal_tail"]
                .as_array()
                .map(|t| t.iter().any(|e| e["entry_type"] == "wait_event"))
                .unwrap_or(false);
            if armed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert!(armed, "run never armed on the wait event");

        let mut cancel_req = req("cancel", &db);
        cancel_req.run_id = Some(run_id.to_string());
        let out = mcp().workflow(Parameters(cancel_req)).await.unwrap();
        assert!(out.contains("cancelled"));

        let status = mcp().workflow(Parameters(status_req())).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&status).unwrap();
        assert_eq!(v["status"], "cancelled");
    }

    #[tokio::test]
    async fn cancel_action_requires_run_id() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");
        let err = mcp()
            .workflow(Parameters(req("cancel", &db)))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("run_id is required"));
    }

    #[tokio::test]
    async fn list_and_status_roundtrip_a_run() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");

        let empty = mcp().workflow(Parameters(req("list", &db))).await.unwrap();
        assert!(empty.contains("[]"), "empty store lists nothing: {empty}");

        // Create a run through the service (async core — this test is already
        // on a runtime) with a deterministic sender, then exercise the MCP
        // handler's status/list against the same store.
        let (run_id, _workflow_id) = crate::workflow::run_workflow_json_async(
            r#"{
                "name": "mcp-pipeline",
                "steps": [
                    {"name": "a", "agent": "opencode", "prompt": "Step A: {{input}}"},
                    {"name": "b", "agent": "opencode", "prompt": "Step B: {{input}}"}
                ]
            }"#,
            "seed",
            &db,
            mock_send(),
        )
        .await
        .unwrap();

        let mut status_req = req("status", &db);
        status_req.run_id = Some(run_id.to_string());
        let status = mcp().workflow(Parameters(status_req)).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&status).unwrap();
        assert_eq!(v["run_id"], run_id.to_string());
        assert_eq!(v["workflow_id"], _workflow_id.to_string());
        assert!(!v["steps"].as_array().unwrap().is_empty());
        assert!(!v["journal_tail"].as_array().unwrap().is_empty());

        let list = mcp().workflow(Parameters(req("list", &db))).await.unwrap();
        assert!(list.contains(&run_id.to_string()));
    }

    #[tokio::test]
    async fn run_resolves_workflow_name_against_the_repo_workflows_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");
        let repo = tempfile::tempdir().unwrap();
        let workflows_dir = crate::workflow::project_workflows_dir(repo.path());
        std::fs::create_dir_all(&workflows_dir).unwrap();
        std::fs::write(
            workflows_dir.join("greet.json"),
            r#"{"name": "greet", "steps": [{"name": "a", "agent": "opencode", "prompt": "hi"}]}"#,
        )
        .unwrap();

        let mcp = AgentflareMcp {
            worktree_repo_root_override: Some(repo.path().to_path_buf()),
            ..Default::default()
        };
        let mut r = req("run", &db);
        r.workflow_name = Some("greet".to_string());
        let out = mcp.workflow(Parameters(r)).await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["run_id"].is_string());
    }

    #[tokio::test]
    async fn run_without_definition_or_workflow_name_is_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");
        let err = mcp()
            .workflow(Parameters(req("run", &db)))
            .await
            .unwrap_err();
        assert!(format!("{err}").contains("definition or workflow_name"));
    }

    #[tokio::test]
    async fn list_definitions_lists_the_repo_workflows_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");
        let repo = tempfile::tempdir().unwrap();
        let workflows_dir = crate::workflow::project_workflows_dir(repo.path());
        std::fs::create_dir_all(&workflows_dir).unwrap();
        std::fs::write(workflows_dir.join("greet.json"), "{}").unwrap();

        let mcp = AgentflareMcp {
            worktree_repo_root_override: Some(repo.path().to_path_buf()),
            ..Default::default()
        };
        let out = mcp
            .workflow(Parameters(req("list_definitions", &db)))
            .await
            .unwrap();
        let names: Vec<String> = serde_json::from_str(&out).unwrap();
        assert_eq!(names, vec!["greet".to_string()]);
    }

    fn write_workflow(repo: &Path, name: &str) {
        let dir = repo.join(".agentflare").join("workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{name}.json")),
            format!(r#"{{"name":"{name}","steps":[{{"name":"s","command":["/bin/true"]}}]}}"#),
        )
        .unwrap();
    }

    fn mcp_at(repo: &Path) -> AgentflareMcp {
        AgentflareMcp {
            worktree_repo_root_override: Some(repo.to_path_buf()),
            ..Default::default()
        }
    }

    #[test]
    fn production_db_path_cannot_redirect_storage() {
        let evil = "/tmp/agentflare-not-the-workflow-store.db";
        let path = resolve_workflow_db_path(Some(evil), true);
        assert_eq!(path, crate::workflow::default_db_path());
        assert_ne!(path, PathBuf::from(evil));
        let test_path = resolve_workflow_db_path(Some(evil), false);
        assert_eq!(test_path, PathBuf::from(evil));
    }

    #[tokio::test]
    async fn schedule_actions_round_trip_on_an_isolated_db() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");
        let repo = tempfile::tempdir().unwrap();
        write_workflow(repo.path(), "echo");
        let mcp = mcp_at(repo.path());

        let mut create = req("schedule_create", &db);
        create.schedule_name = Some("nightly".into());
        create.workflow_name = Some("echo".into());
        create.every = Some("2m".into());
        create.grace = Some("30s".into());
        create.params = Some(r#"{"who":"ada"}"#.into());
        let created: serde_json::Value =
            serde_json::from_str(&mcp.workflow(Parameters(create)).await.unwrap()).unwrap();
        assert_eq!(created["name"], "nightly");
        assert_eq!(created["every_secs"], 120);
        assert_eq!(created["effective_grace_secs"], 30);
        assert_eq!(created["params"]["who"], "ada");
        let id = created["id"].as_str().unwrap().to_string();

        let listed: serde_json::Value = serde_json::from_str(
            &mcp.workflow(Parameters(req("schedule_list", &db)))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);

        let mut show = req("schedule_show", &db);
        show.schedule = Some(id.clone());
        let shown: serde_json::Value =
            serde_json::from_str(&mcp.workflow(Parameters(show)).await.unwrap()).unwrap();
        assert_eq!(shown["workflow_name"], "echo");

        let mut update = req("schedule_update", &db);
        update.schedule = Some("nightly".into());
        update.every = Some("3m".into());
        update.overlap = Some("allow".into());
        let updated: serde_json::Value =
            serde_json::from_str(&mcp.workflow(Parameters(update)).await.unwrap()).unwrap();
        assert_eq!(updated["every_secs"], 180);
        assert_eq!(updated["overlap"], "allow");

        let mut disable = req("schedule_disable", &db);
        disable.schedule = Some(id.clone());
        let disabled: serde_json::Value =
            serde_json::from_str(&mcp.workflow(Parameters(disable)).await.unwrap()).unwrap();
        assert_eq!(disabled["enabled"], false);

        let mut enable = req("schedule_enable", &db);
        enable.schedule = Some("nightly".into());
        let enabled: serde_json::Value =
            serde_json::from_str(&mcp.workflow(Parameters(enable)).await.unwrap()).unwrap();
        assert_eq!(enabled["enabled"], true);

        let mut fire = req("schedule_fire", &db);
        fire.schedule = Some("nightly".into());
        let receipt: serde_json::Value =
            serde_json::from_str(&mcp.workflow(Parameters(fire)).await.unwrap()).unwrap();
        assert_eq!(receipt["status"], "queued");
        assert!(receipt["occurrence_id"].is_string());

        let runs_n: i64 = rusqlite::Connection::open(&db)
            .unwrap()
            .query_row("SELECT COUNT(*) FROM workflow_runs", [], |r| r.get(0))
            .unwrap();
        assert_eq!(runs_n, 0, "schedule_fire must not start a workflow");

        let mut runs = req("schedule_runs", &db);
        runs.schedule = Some(id.clone());
        let history: serde_json::Value =
            serde_json::from_str(&mcp.workflow(Parameters(runs)).await.unwrap()).unwrap();
        assert_eq!(history.as_array().unwrap()[0]["status"], "queued");
        assert!(history.as_array().unwrap()[0]["run_id"].is_null());

        let mut remove = req("schedule_remove", &db);
        remove.schedule = Some(id);
        let gone = mcp.workflow(Parameters(remove)).await.unwrap();
        assert!(gone.contains("removed"));

        let mut missing = req("schedule_show", &db);
        missing.schedule = Some("nightly".into());
        assert!(mcp.workflow(Parameters(missing)).await.is_err());
    }

    #[tokio::test]
    async fn schedule_list_is_scoped_and_validation_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");
        let repo_a = tempfile::tempdir().unwrap();
        let repo_b = tempfile::tempdir().unwrap();
        write_workflow(repo_a.path(), "echo");
        let mcp_a = mcp_at(repo_a.path());
        let mcp_b = mcp_at(repo_b.path());

        let mut create = req("schedule_create", &db);
        create.schedule_name = Some("nightly".into());
        create.workflow_name = Some("echo".into());
        create.every = Some("1m".into());
        let created: serde_json::Value =
            serde_json::from_str(&mcp_a.workflow(Parameters(create)).await.unwrap()).unwrap();
        let id = created["id"].as_str().unwrap();

        let listed: serde_json::Value = serde_json::from_str(
            &mcp_b
                .workflow(Parameters(req("schedule_list", &db)))
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(listed.as_array().unwrap().is_empty());

        let mut all = req("schedule_list", &db);
        all.all = true;
        let everyone: serde_json::Value =
            serde_json::from_str(&mcp_b.workflow(Parameters(all)).await.unwrap()).unwrap();
        assert_eq!(everyone.as_array().unwrap().len(), 1);

        let mut show = req("schedule_show", &db);
        show.schedule = Some(id.into());
        assert!(mcp_b.workflow(Parameters(show)).await.is_ok());

        let mut bad = req("schedule_create", &db);
        bad.schedule_name = Some("other".into());
        bad.workflow_name = Some("echo".into());
        bad.every = Some("30s".into());
        let err = mcp_a.workflow(Parameters(bad)).await.unwrap_err();
        assert!(format!("{err}").contains("minimum is 60"));

        let mut reserved = req("schedule_create", &db);
        reserved.schedule_name = Some("other".into());
        reserved.workflow_name = Some("echo".into());
        reserved.every = Some("1m".into());
        reserved.params = Some(r#"{"schedule":1}"#.into());
        let err = mcp_a.workflow(Parameters(reserved)).await.unwrap_err();
        assert!(format!("{err}").contains("reserves top-level key `schedule`"));
    }

    #[tokio::test]
    async fn schedule_create_normalizes_linked_worktree_override_to_main_checkout() {
        let parent = tempfile::tempdir().unwrap();
        let main = parent.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        let git = |dir: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&main, &["init", "-b", "master"]);
        git(&main, &["config", "user.email", "test@test.com"]);
        git(&main, &["config", "user.name", "Test"]);
        git(&main, &["commit", "--allow-empty", "-m", "initial"]);
        let wt = parent.path().join("wt");
        git(
            &main,
            &["worktree", "add", "-b", "wt-branch", wt.to_str().unwrap()],
        );
        write_workflow(&main, "echo");

        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("wf.db");
        let mut create = req("schedule_create", &db);
        create.schedule_name = Some("nightly".into());
        create.workflow_name = Some("echo".into());
        create.every = Some("1m".into());
        let created: serde_json::Value =
            serde_json::from_str(&mcp_at(&wt).workflow(Parameters(create)).await.unwrap()).unwrap();
        let main_canon = std::fs::canonicalize(&main).unwrap();
        let wt_canon = std::fs::canonicalize(&wt).unwrap();
        assert_eq!(created["repo_root"], main_canon.to_str().unwrap());
        assert_ne!(created["repo_root"], wt_canon.to_str().unwrap());
    }
}
