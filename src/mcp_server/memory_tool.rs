//! `memory` MCP tool handler body -- split out of mcp_server.rs (item #168).

use super::*;

impl AgentflareMcp {
    pub fn memory_impl(&self, req: MemoryRequest) -> Result<String, ErrorData> {
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
}
