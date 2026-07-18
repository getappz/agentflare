//! `artifact` MCP tool handler body -- split out of mcp_server.rs (item #168).

use super::*;

impl AgentflareMcp {
    pub fn artifact_impl(&self, req: ArtifactRequest) -> Result<String, ErrorData> {
        match req.action.as_str() {
            "publish" => {
                let name = req
                    .name
                    .ok_or_else(|| ErrorData::invalid_params("name is required", None))?;
                if name.trim().is_empty() {
                    return Err(ErrorData::invalid_params("name is required", None));
                }
                let content = req
                    .content
                    .ok_or_else(|| ErrorData::invalid_params("content is required", None))?;
                if content.is_empty() {
                    return Err(ErrorData::invalid_params("content is required", None));
                }
                let (store, base) = self.ensure_artifact_server()?;
                let req2 = agentflare_artifacts::PublishRequest {
                    name,
                    artifact_type: agentflare_artifacts::ArtifactType::from(
                        req.r#type.as_deref().unwrap_or("text"),
                    ),
                    content,
                    session_id: req.session_id.unwrap_or_default(),
                    update_id: req.update_id,
                    label: req.label,
                    description: req.description,
                    favicon: req.favicon,
                    base_version: req.base_version,
                    sender: self.agent.clone(),
                    recipient: req.recipient,
                    thread_id: req.thread_id,
                    reply_to: req.reply_to,
                    git: Self::git_provenance(),
                };
                let resp = store.publish(&req2).map_err(Self::artifact_error)?;
                Ok(serde_json::to_string_pretty(&serde_json::json!({ "id": resp.id, "version": resp.version, "url": format!("{base}/{}", resp.id), "index": format!("{base}/") })).unwrap_or_default())
            }
            "list" => {
                let (store, base) = self.ensure_artifact_server()?;
                let summaries = store
                    .list(req.session_id.as_deref())
                    .map_err(Self::artifact_error)?;
                let items: Vec<serde_json::Value> = summaries
                    .iter()
                    .filter(|s| {
                        req.inbox_recipient
                            .as_deref()
                            .is_none_or(|r| s.recipient.as_deref() == Some(r))
                            && req
                                .thread_id
                                .as_deref()
                                .is_none_or(|t| s.thread_id.as_deref() == Some(t))
                    })
                    .map(|s| {
                        let mut v = serde_json::to_value(s).unwrap_or_default();
                        if let Some(obj) = v.as_object_mut() {
                            obj.insert("url".into(), serde_json::json!(format!("{base}/{}", s.id)));
                        }
                        v
                    })
                    .collect();
                Ok(serde_json::to_string_pretty(&items).unwrap_or_default())
            }
            "get" => {
                let id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required", None))?;
                let (store, _) = self.ensure_artifact_server()?;
                let artifact = match req.version {
                    Some(n) => store.get_version(&id, n),
                    None => store.get(&id),
                }
                .map_err(Self::artifact_error)?;
                Ok(serde_json::to_string_pretty(&artifact).unwrap_or_default())
            }
            "diff" => {
                let id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required", None))?;
                let from_version = req
                    .from_version
                    .ok_or_else(|| ErrorData::invalid_params("from_version is required", None))?;
                let (store, _) = self.ensure_artifact_server()?;
                let to = match req.to_version {
                    Some(v) => v,
                    None => store.get(&id).map_err(Self::artifact_error)?.version,
                };
                let diff = store
                    .diff(&id, from_version, to)
                    .map_err(Self::artifact_error)?;
                Ok(serde_json::to_string_pretty(&diff).unwrap_or_default())
            }
            "search" => {
                let query = req
                    .query
                    .ok_or_else(|| ErrorData::invalid_params("query is required", None))?;
                if query.trim().is_empty() {
                    return Err(ErrorData::invalid_params("query is required", None));
                }
                let (store, base) = self.ensure_artifact_server()?;
                let needle = query.to_lowercase();
                let mut hits = Vec::new();
                for summary in store
                    .list(req.session_id.as_deref())
                    .map_err(Self::artifact_error)?
                {
                    let name_hit = summary.name.to_lowercase().contains(&needle);
                    let desc_hit = summary
                        .description
                        .as_deref()
                        .is_some_and(|d| d.to_lowercase().contains(&needle));
                    let content = store
                        .get(&summary.id)
                        .map(|a| a.content)
                        .unwrap_or_default();
                    let content_pos = content.to_lowercase().find(&needle);
                    if !(name_hit || desc_hit || content_pos.is_some()) {
                        continue;
                    }
                    let snippet = content_pos.map(|pos| {
                        let mut start = pos.saturating_sub(40);
                        while !content.is_char_boundary(start) {
                            start -= 1;
                        }
                        let mut end = (pos + needle.len() + 40).min(content.len());
                        while !content.is_char_boundary(end) {
                            end += 1;
                        }
                        content[start..end].to_string()
                    });
                    let mut v = serde_json::to_value(&summary).unwrap_or_default();
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert(
                            "url".into(),
                            serde_json::json!(format!("{base}/{}", summary.id)),
                        );
                        if let Some(snippet) = snippet {
                            obj.insert("snippet".into(), serde_json::json!(snippet));
                        }
                    }
                    hits.push(v);
                }
                Ok(serde_json::to_string_pretty(&hits).unwrap_or_default())
            }
            "delete" => {
                let id = req
                    .id
                    .ok_or_else(|| ErrorData::invalid_params("id is required", None))?;
                let (store, _) = self.ensure_artifact_server()?;
                store.delete(&id).map_err(Self::artifact_error)?;
                Ok(serde_json::json!({"deleted": id}).to_string())
            }
            other => Err(ErrorData::invalid_params(
                format!("unknown action: {other}"),
                None,
            )),
        }
    }
}
