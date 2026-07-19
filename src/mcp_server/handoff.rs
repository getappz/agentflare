//! `handoff` MCP tool handler body -- split out of mcp_server.rs (item #168).

use super::*;

impl AgentflareMcp {
    pub fn handoff_impl(
        &self,
        HandoffRequest {
            recipient,
            name,
            content,
            r#type,
            item_id,
            thread_id,
            reply_to,
            description,
            facts,
            summary,
            findings,
            decisions,
            files_touched,
            evidence,
        }: HandoffRequest,
    ) -> Result<String, ErrorData> {
        if recipient.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "recipient is required for a handoff — without it the item lands with no assignee",
                None,
            ));
        }
        if name.trim().is_empty() {
            return Err(ErrorData::invalid_params("name is required", None));
        }
        if content.is_empty() {
            return Err(ErrorData::invalid_params("content is required", None));
        }
        let recipient = recipient.trim().to_string();
        let name = name.trim().to_string();
        let ext = match r#type.as_deref() {
            Some("html") => "html",
            Some("mermaid") | Some("diagram") => "mmd",
            Some("text") => "txt",
            _ => "md",
        };

        self.with_backend_db(|conn| {
            let project = self.resolve_project(conn)?;
            let ws_id = Self::resolve_workspace_id(conn)?;

            let item = match &item_id {
                Some(id) => {
                    let input = agentflare_backend::item::UpdateItem {
                        assignee_agent: Some(recipient.clone()),
                        ..Default::default()
                    };
                    agentflare_backend::item::update(conn, id, input).map_err(map_backend_err)?
                }
                None => {
                    let state_id = agentflare_backend::state::list_by_project(conn, &project.id)
                        .map_err(map_backend_err)?
                        .into_iter()
                        .find(|s| s.is_default)
                        .ok_or_else(|| {
                            ErrorData::internal_error("project has no default state", None)
                        })?
                        .id;
                    let metadata = thread_id
                        .as_ref()
                        .map(|t| serde_json::json!({ "thread": t }).to_string());
                    let input = agentflare_backend::item::CreateItem {
                        project_id: project.id.clone(),
                        state_id,
                        name: name.clone(),
                        description: description.clone().or_else(|| Some(content.clone())),
                        priority: None,
                        parent_id: None,
                        assignee_agent: Some(recipient.clone()),
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                        metadata,
                        label_ids: vec![],
                        assignee_ids: vec![],
                        dependency_ids: vec![],
                    };
                    agentflare_backend::item::create(conn, input).map_err(map_backend_err)?
                }
            };

            let bytes = content.as_bytes();
            let hash = Self::content_hash(bytes);
            // Keyed on item.id, not name — name is the per-call brief and
            // can legitimately differ between messages on the same item
            // (e.g. a reply's brief vs. the original ask); keying on it
            // would silently reset versioning to 1 instead of continuing
            // the chain.
            let safe_stem = Self::slugify(&item.id);
            let filename = format!("{safe_stem}.{ext}");
            let full_storage = format!("{ws_id}/assets/{safe_stem}-{hash}.{ext}");
            let base_path = crate::paths::home().join(".agentflare");
            let target = base_path.join(&full_storage);
            if !target.exists() {
                agentflare_backend::asset::write_file(&base_path, &full_storage, bytes)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            }
            let mut meta = serde_json::json!({ "sender": self.agent, "recipient": recipient });
            if let Some(t) = &thread_id {
                meta["thread_id"] = serde_json::json!(t);
            }
            if let Some(r) = &reply_to {
                meta["reply_to"] = serde_json::json!(r);
            }
            // Session snapshot — lets the recipient see context without extra round-trips.
            if let Some(s) = summary {
                meta["session_summary"] = serde_json::json!(s);
            }
            if let Some(f) = findings {
                meta["findings"] = serde_json::json!(f);
            }
            if let Some(d) = decisions {
                meta["decisions"] = serde_json::json!(d);
            }
            if let Some(f) = files_touched {
                meta["files_touched"] = serde_json::json!(f);
            }
            if let Some(e) = evidence {
                meta["evidence"] = serde_json::json!(e);
            }
            let asset = agentflare_backend::asset::create(
                conn,
                agentflare_backend::asset::CreateAsset {
                    workspace_id: Some(ws_id),
                    entity_type: "item_attachment".into(),
                    entity_id: item.id.clone(),
                    filename,
                    size: bytes.len() as i64,
                    mime_type: Some(Self::infer_mime_type(ext)),
                    metadata: Some(meta.to_string()),
                    storage_path: Some(full_storage),
                },
            )
            .map_err(map_backend_err)?;

            // Knowledge fact import: persist each fact into the recipient's memory,
            // scoped to this handoff's project. Runs only after the item/asset are
            // committed, so a failed handoff never leaves orphaned facts behind.
            if let Some(ref facts) = facts {
                let sender = self.agent.as_deref().unwrap_or("unknown");
                for fact in facts {
                    let title = fact
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("handoff fact");
                    let body = fact.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    let fact_type = fact
                        .get("type")
                        .and_then(|v| v.as_str())
                        .unwrap_or("discovery");
                    if body.is_empty() {
                        continue;
                    }
                    let input = crate::memory::mcp::RememberInput {
                        title: format!("[{sender}] {title}"),
                        content: body.to_string(),
                        r#type: fact_type.to_string(),
                        session_id: None,
                        project: Some(project.id.clone()),
                        topic_key: fact
                            .get("topic_key")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string()),
                        scope: None,
                    };
                    if let Err(e) = crate::memory::mcp::handle_remember(input) {
                        // Fact import is best-effort — never fail the handoff for a memory write
                        eprintln!("[handoff] fact import failed: {e}");
                    }
                }
            }

            let result = serde_json::json!({
                "item_id": item.id,
                "item_sequence_id": item.sequence_id,
                "asset_id": asset.id,
                "asset_version": asset.version,
                "recipient": recipient,
            });
            Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
        })?
    }
}
