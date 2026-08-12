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
                let (run_id, workflow_id) =
                    crate::workflow::run_workflow_json(&definition, &input, &db_path)
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
                let status = crate::workflow::workflow_status(&run_id, &db_path)
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
                crate::workflow::complete_workflow_event(&run_id, &name, &result, &db_path)
                    .map_err(|e| ErrorData::invalid_params(e, None))?;
                Ok(r#"{"status":"completed"}"#.to_string())
            }
            "list" => {
                let runs = crate::workflow::list_workflows(&db_path)
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
