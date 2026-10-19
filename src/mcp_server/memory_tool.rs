//! `memory` MCP tool handler body -- split out of mcp_server.rs (item #168).

use super::*;

impl AgentflareMcp {
    pub async fn memory_impl(&self, req: MemoryRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "remember" => {
                let title = req
                    .title
                    .ok_or_else(|| ErrorData::invalid_params("title is required", None))?;
                let content = req
                    .content
                    .ok_or_else(|| ErrorData::invalid_params("content is required", None))?;
                let r#type = req
                    .r#type
                    .ok_or_else(|| ErrorData::invalid_params("type is required", None))?;
                let input = crate::memory::mcp::RememberInput {
                    title,
                    content,
                    r#type,
                    session_id: req.session_id,
                    project: req.project,
                    topic_key: req.topic_key,
                    scope: req.scope,
                };
                crate::memory::mcp::handle_remember(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            "recall" => {
                let input = crate::memory::mcp::RecallInput {
                    query: req.query,
                    id: req.id,
                    r#type: req.r#type,
                    project: req.project,
                    limit: req.limit,
                };
                crate::memory::mcp::handle_recall(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            "context" => {
                let input = crate::memory::mcp::ContextInput {
                    session_id: req.session_id,
                    project: req.project,
                };
                crate::memory::mcp::handle_context(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            "handoff" => {
                let session_id = req
                    .session_id
                    .ok_or_else(|| ErrorData::invalid_params("session_id is required", None))?;
                let summary = req
                    .summary
                    .ok_or_else(|| ErrorData::invalid_params("summary is required", None))?;
                let input = crate::memory::mcp::HandoffInput {
                    session_id,
                    summary,
                    findings: req.findings,
                    decisions: req.decisions,
                    files_touched: req.files_touched,
                    evidence: req.evidence,
                    completed: req.completed,
                    remaining: req.remaining,
                    source_ref: req.source_ref,
                    dropped_fields: req.dropped_fields,
                };
                crate::memory::mcp::handle_handoff(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            "compact" => {
                let input = crate::memory::mcp::CompactInput {
                    lines: req.content.unwrap_or_default(),
                    query: req.query,
                    compression_ratio: req.compression_ratio,
                    preserve_recent: req.preserve_recent,
                    scorer: req.scorer,
                };
                crate::memory::mcp::handle_compact(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            "relate" => {
                let source_id = req
                    .source_id
                    .ok_or_else(|| ErrorData::invalid_params("source_id is required", None))?;
                let target_id = req
                    .target_id
                    .ok_or_else(|| ErrorData::invalid_params("target_id is required", None))?;
                let relation = req
                    .relation
                    .ok_or_else(|| ErrorData::invalid_params("relation is required", None))?;
                let input = crate::memory::mcp::RelateInput {
                    source_id,
                    target_id,
                    relation,
                    reason: req.reason,
                    confidence: req.confidence,
                };
                crate::memory::mcp::handle_relate(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            "promote" => {
                let title = req
                    .title
                    .ok_or_else(|| ErrorData::invalid_params("title is required", None))?;
                let content = req
                    .content
                    .ok_or_else(|| ErrorData::invalid_params("content is required", None))?;
                let run_id = req
                    .run_id
                    .filter(|id| !id.trim().is_empty())
                    .ok_or_else(|| {
                        ErrorData::invalid_params(
                            "run_id is required for evidence-backed promotion",
                            None,
                        )
                    })?;
                let project = req
                    .project
                    .filter(|p| !p.trim().is_empty())
                    .ok_or_else(|| ErrorData::invalid_params("project is required", None))?;
                let report = crate::workflow::workflow_report_async(
                    &run_id,
                    &crate::workflow::default_db_path(),
                )
                .await
                .map_err(|e| ErrorData::invalid_params(e, None))?;
                let project = self.promotion_project(&report, &project)?;
                let input = crate::memory::mcp::PromoteInput {
                    title,
                    content,
                    project: Some(project),
                    evidence_ids: req.evidence_ids.unwrap_or_default(),
                    confidence: req.confidence,
                    session_id: req.session_id,
                    topic_key: req.topic_key,
                    run_id,
                };
                crate::memory::mcp::handle_promote(input, &report)
                    .map_err(|e| ErrorData::invalid_params(e, None))
            }
            "curate" => {
                let id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required", None))?;
                let curate_action = req.curate_action.ok_or_else(|| {
                    ErrorData::invalid_params(
                        "curate_action is required (update|delete|pin|unpin)",
                        None,
                    )
                })?;
                let input = crate::memory::mcp::CurateInput {
                    action: curate_action,
                    id,
                    title: req.title,
                    content: req.content,
                    r#type: req.r#type,
                    pinned: req.pinned,
                };
                crate::memory::mcp::handle_curate(input)
                    .map_err(|e| ErrorData::internal_error(e, None))
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    }

    pub(super) fn promotion_project(
        &self,
        report: &serde_json::Value,
        requested: &str,
    ) -> Result<String, ErrorData> {
        let item_id = report["work_item"]["item_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ErrorData::invalid_params("run has no project-scoped work item provenance", None)
            })?;
        self.with_backend_db(|conn| {
            let project = self.resolve_project_for_read(conn, Some(requested))?;
            let item = agentflare_backend::item::get(conn, item_id).map_err(map_backend_err)?;
            if item.project_id != project.id {
                return Err(ErrorData::invalid_params(
                    "evidence run belongs to a different project",
                    None,
                ));
            }
            Ok(project.name)
        })?
    }
}
