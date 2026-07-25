//! `skill` MCP tool handler body — split out of mcp_server.rs, mirroring
//! item.rs/claim.rs/comment.rs's one-file-per-tool convention.

use super::*;

impl AgentflareMcp {
    pub(super) async fn skill_impl(&self, req: SkillRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "search" => {
                let query = req
                    .query
                    .ok_or_else(|| ErrorData::invalid_params("query is required", None))?;
                if query.trim().is_empty() {
                    return Err(ErrorData::invalid_params("query is required", None));
                }
                let mode = match req.mode.as_deref() {
                    None | Some("all") => skill_registry::MatchMode::All,
                    Some("any") => skill_registry::MatchMode::Any,
                    Some(other) => {
                        return Err(ErrorData::invalid_params(
                            format!("mode must be 'all' or 'any', got '{other}'"),
                            None,
                        ));
                    }
                };
                let limit = req.limit.unwrap_or(5);
                let local = self
                    .with_fresh_registry(|reg| reg.search(&query, limit, mode))?
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let hits = if local.len() < limit {
                    let remaining = limit - local.len();
                    let query_owned = query.clone();
                    let registry = tokio::task::spawn_blocking(move || {
                        gateway_registry::registry_search::search_registry(&query_owned, remaining)
                    })
                    .await
                    .unwrap_or_default();
                    skill_registry::merge_registry_hits(local, limit, registry)
                } else {
                    local
                };
                Ok(serde_json::to_string_pretty(&hits).unwrap_or_default())
            }
            "load" => {
                let name = req
                    .name
                    .ok_or_else(|| ErrorData::invalid_params("name is required", None))?;
                if name.trim().is_empty() {
                    return Err(ErrorData::invalid_params("name is required", None));
                }
                let result = self.with_fresh_registry(|reg| reg.load(&name, req.original))?;
                match result {
                    Ok(s) => {
                        let json = serde_json::to_string_pretty(&s).unwrap_or_default();
                        if req.activation_wrapper {
                            let siblings: Vec<String> = s
                                .siblings
                                .iter()
                                .map(|p| p.to_string_lossy().to_string())
                                .collect();
                            let siblings_block = if siblings.is_empty() {
                                String::new()
                            } else {
                                format!("\n\nCompanion scripts:\n- {}", siblings.join("\n- "))
                            };
                            Ok(format!(
                                "<SKILL_ACTIVATION>\nFollow this skill definition verbatim:{json}{siblings_block}\n</SKILL_ACTIVATION>"
                            ))
                        } else {
                            Ok(json)
                        }
                    }
                    Err(e @ skill_registry::LoadError::NotFound(_))
                    | Err(e @ skill_registry::LoadError::Ambiguous(_)) => {
                        Err(ErrorData::invalid_params(e.to_string(), None))
                    }
                    Err(e) => Err(ErrorData::internal_error(e.to_string(), None)),
                }
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    }
}
