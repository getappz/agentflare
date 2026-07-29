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
            last_commit,
            completed,
            remaining,
            blockers,
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

            if let Some(oid) = &last_commit {
                self.verify_continuation_commit(conn, oid, item_id.as_deref())?;
            }

            let item = match &item_id {
                Some(id) => {
                    let input = agentflare_backend::item::UpdateItem {
                        assignee_agent: Some(recipient.clone()),
                        ..Default::default()
                    };
                    agentflare_backend::item::update(conn, id, input).map_err(map_backend_err)?
                }
                None => {
                    // Reuse an existing open item already assigned to the
                    // recipient with a matching name or thread, instead of
                    // blindly creating a duplicate — the actual fix for the
                    // "handoff creates a duplicate item" bug on the
                    // reply/continuation path. Genuinely new work (no match)
                    // still auto-creates, unchanged.
                    let canonical_recipient = agent_registry::canonicalize(&recipient);
                    let reusable = agentflare_backend::item::list_by_assignee_agent(
                        conn,
                        &project.id,
                        &canonical_recipient,
                    )
                    .map_err(map_backend_err)?
                    .into_iter()
                    .find(|i| {
                        i.name == name
                            || thread_id.as_ref().is_some_and(|t| {
                                serde_json::from_str::<serde_json::Value>(&i.metadata)
                                    .ok()
                                    .and_then(|m| {
                                        m.get("thread")
                                            .and_then(|v| v.as_str())
                                            .map(str::to_string)
                                    })
                                    .as_deref()
                                    == Some(t.as_str())
                            })
                    });
                    if let Some(item) = reusable {
                        item
                    } else {
                        let state_id =
                            agentflare_backend::state::list_by_project(conn, &project.id)
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
                }
            };

            let bytes = content.as_bytes();
            let safe_stem = Self::slugify(&item.id);
            let asset_id = db_kit::ids::new_id();
            let filename = format!("{safe_stem}-{asset_id}.{ext}");
            let entity_path =
                crate::asset_store::entity_path("item_attachment", &item.id, &filename);
            let mut meta = serde_json::json!({
                "sender": self.agent,
                "recipient": recipient,
                "completed": completed,
                "remaining": remaining,
            });
            if let Some(t) = &thread_id {
                meta["thread_id"] = serde_json::json!(t);
            }
            if let Some(r) = &reply_to {
                meta["reply_to"] = serde_json::json!(r);
            }
            if let Some(oid) = &last_commit {
                meta["last_commit"] = serde_json::json!(oid);
            }
            if let Some(b) = blockers {
                meta["blockers"] = serde_json::json!(b);
            }
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

            let mime_type = Self::infer_mime_type(ext);

            let result = self.with_store(|store| -> Result<serde_json::Value, ErrorData> {
                let prefix = format!("item_attachment/{}", item.id);
                let existing = store
                    .doc_list(&ws_id)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
                let version = existing
                    .iter()
                    .filter(|d| d.path.starts_with(&prefix))
                    .count() as i32
                    + 1;

                let blob_hash = store
                    .blob_store(bytes)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

                let doc = store
                    .doc_upsert_with_opts(
                        &ws_id,
                        &entity_path,
                        "",
                        agentflare_store::documents::DocUpsertOpts {
                            title: Some(filename.clone()),
                            doc_type: Some("asset".into()),
                            blob_hash: Some(blob_hash),
                            mime: Some(mime_type.clone()),
                            source: Some("handoff".into()),
                            metadata: Some(meta.to_string()),
                            size: Some(bytes.len() as i64),

                            ..Default::default()
                        },
                    )
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

                Ok(serde_json::json!({
                    "item_id": item.id,
                    "item_sequence_id": item.sequence_id,
                    "asset_id": doc.id,
                    "asset_version": version,
                    "recipient": recipient,
                }))
            })??;

            // Knowledge fact import: persist each fact into the recipient's memory
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
                        eprintln!("[handoff] fact import failed: {e}");
                    }
                }
            }

            Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
        })?
    }

    /// Verified, not trusted: rejects a fabricated or typo'd continuation
    /// OID rather than recording it as-is. `oid` must exist in the repo;
    /// when the target item's own `task/<seq>` branch already exists
    /// locally, `oid` must additionally be reachable from it — a handoff
    /// can't claim to continue from a commit that branch never saw.
    fn verify_continuation_commit(
        &self,
        conn: &rusqlite::Connection,
        oid: &str,
        item_id: Option<&str>,
    ) -> Result<(), ErrorData> {
        let repo_root = self.worktree_repo_root();
        if !flare_git_core::shell::run_in_ok(&repo_root, &["cat-file", "-e", oid]) {
            return Err(ErrorData::invalid_params(
                format!("last_commit '{oid}' does not exist in this repo — verified, not trusted"),
                None,
            ));
        }
        let Some(id) = item_id else {
            return Ok(());
        };
        let Ok(item) = agentflare_backend::item::get(conn, id) else {
            return Ok(()); // item resolution/existence is checked separately below
        };
        let branch = format!("task/{}", item.sequence_id);
        let branch_exists = flare_git_core::shell::run_in_ok(
            &repo_root,
            &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")],
        );
        if branch_exists
            && !flare_git_core::shell::run_in_ok(&repo_root, &["merge-base", "--is-ancestor", oid, &branch])
        {
            return Err(ErrorData::invalid_params(
                format!(
                    "last_commit '{oid}' is not reachable from '{branch}' — verified, not trusted"
                ),
                None,
            ));
        }
        Ok(())
    }
}
