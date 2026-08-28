//! `workflow` MCP tool handler — one-file-per-tool convention (mirrors
//! item.rs/claim.rs/comment.rs). Thin wrapper over `crate::workflow` service:
//! run/status/complete_event/list for durable agent pipelines.

use std::path::PathBuf;

use super::*;

impl AgentflareMcp {
    pub(super) async fn workflow_impl(&self, req: WorkflowRequest) -> Result<String, ErrorData> {
        // `db_path` is a test-only override so fixtures can use isolated
        // tempdirs (see the tests below); an MCP caller-supplied path is
        // never honored in production, since that would let any client
        // point the workflow store at an arbitrary file on disk.
        let db_path = if cfg!(test) {
            req.db_path
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(crate::workflow::default_db_path)
        } else {
            crate::workflow::default_db_path()
        };

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
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    }
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
}
