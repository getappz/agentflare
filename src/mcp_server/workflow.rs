//! `workflow` MCP tool handler — one-file-per-tool convention (mirrors
//! item.rs/claim.rs/comment.rs). Thin wrapper over `crate::workflow` service:
//! run/status/complete_event/list for durable agent pipelines.

use std::path::PathBuf;

use super::*;

impl AgentflareMcp {
    pub(super) async fn workflow_impl(&self, req: WorkflowRequest) -> Result<String, ErrorData> {
        let db_path = req
            .db_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(crate::workflow::default_db_path);

        match req.action.as_str() {
            "run" => {
                let definition = req.definition.ok_or_else(|| {
                    ErrorData::invalid_params("definition (JSON workflow) is required", None)
                })?;
                let input = req.input.unwrap_or_default();
                let (run_id, workflow_id) = crate::workflow::run_workflow_json_async(
                    &definition,
                    &input,
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
            "list" => {
                let runs = crate::workflow::list_workflows_async(&db_path)
                    .await
                    .map_err(|e| ErrorData::internal_error(e, None))?;
                Ok(serde_json::to_string_pretty(&runs).unwrap_or_default())
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
        Arc::new(|agent: String, prompt: String| {
            Box::pin(async move { Ok((format!("[{agent}] {prompt}"), 1, 1)) })
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
}
