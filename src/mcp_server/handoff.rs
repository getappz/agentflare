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
        if completed.trim().is_empty() || remaining.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "completed and remaining are required — an empty structured payload tells the recipient nothing",
                None,
            ));
        }
        let recipient = recipient.trim().to_string();
        let name = name.trim().to_string();
        let ext = match r#type.as_deref() {
            Some("html") => "html",
            Some("mermaid") | Some("diagram") => "mmd",
            Some("text") => "txt",
            _ => "md",
        };

        // Resolve just the target branch (a DB read) under the backend
        // lock, then run the blocking git subprocess checks below it —
        // `verify_continuation_commit` has no business running while the
        // shared DB mutex is held (same reasoning as `item_claim`'s split
        // of DB resolution from `git worktree add`).
        if let Some(oid) = &last_commit {
            let branch = match &item_id {
                Some(id) => self.with_backend_db(|conn| {
                    agentflare_backend::item::get(conn, id)
                        .ok()
                        .map(|item| format!("task/{}", item.sequence_id))
                })?,
                None => None,
            };
            self.verify_continuation_commit(oid, branch.as_deref())?;
        }

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
                                        m.get("thread").and_then(|v| v.as_str()).map(str::to_string)
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
                        // A brand-new handed-off item is real, undone work —
                        // labeling it `ready-for-work` (when the project has
                        // that label at all; skipped otherwise rather than
                        // creating it out of nowhere) lets the supervisor's
                        // discovery loop pick it up without a human doing
                        // that by hand. Its own `resolve_confirmed_agent`
                        // gate already handles a non-autonomous recipient
                        // safely (relabels to `needs-manual-dispatch` with an
                        // explanatory comment), so that's not duplicated
                        // here. Reply/continuation items (the `reusable`
                        // branch above) are deliberately left alone — they
                        // may already be claimed, in progress, or done, and
                        // silently re-queuing those for dispatch would be
                        // wrong.
                        let ready_label_id =
                            agentflare_backend::label::list_by_project(conn, &project.id)
                                .ok()
                                .and_then(|labels| {
                                    labels
                                        .into_iter()
                                        .find(|l| l.name == crate::supervisor::READY_LABEL)
                                })
                                .map(|l| l.id);
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
                            label_ids: ready_label_id.into_iter().collect(),
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
    /// OID rather than recording it as-is. `oid` must exist in the repo as
    /// a commit (not just any object); when `branch` is given and exists,
    /// `oid` must additionally be reachable from it — a handoff can't claim
    /// to continue from a commit that branch never saw. `branch` must be
    /// resolved by the caller *before* calling this and outside
    /// `with_backend_db` — this is pure git subprocess work (up to three
    /// blocking calls) that has no business running while the shared
    /// backend DB mutex is held.
    fn verify_continuation_commit(&self, oid: &str, branch: Option<&str>) -> Result<(), ErrorData> {
        // Reject anything that isn't a plain hex OID before it ever reaches
        // git: a leading '-' would otherwise be parsed as an option by
        // `cat-file`/`merge-base` rather than as a rev.
        if oid.len() < 7 || !oid.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ErrorData::invalid_params(
                format!("last_commit '{oid}' is not a valid object id"),
                None,
            ));
        }
        let repo_root = self.worktree_repo_root();
        // `^{commit}` forces commit-type resolution — plain `cat-file -e`
        // also succeeds for blobs/trees/tags, which aren't valid
        // continuation points.
        let commit_ref = format!("{oid}^{{commit}}");
        if !flare_git_core::shell::run_in_ok(&repo_root, &["cat-file", "-e", &commit_ref]) {
            return Err(ErrorData::invalid_params(
                format!("last_commit '{oid}' does not exist in this repo — verified, not trusted"),
                None,
            ));
        }
        let Some(branch) = branch else {
            return Ok(());
        };
        let branch_ref = format!("refs/heads/{branch}");
        let branch_exists = flare_git_core::shell::run_in_ok(
            &repo_root,
            &["show-ref", "--verify", "--quiet", &branch_ref],
        );
        if branch_exists
            && !flare_git_core::shell::run_in_ok(
                &repo_root,
                &["merge-base", "--is-ancestor", oid, branch],
            )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp_server::types::HandoffRequest;

    fn init_test_repo(root: &std::path::Path) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
        };
        run(&["init", "-b", "master"]);
        run(&["config", "user.email", "test@test.com"]);
        run(&["config", "user.name", "Test"]);
        run(&["commit", "--allow-empty", "-m", "initial"]);
    }

    fn test_mcp() -> (tempfile::TempDir, AgentflareMcp) {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        init_test_repo(&repo_root);
        let backend_db = tmp.path().join("backend.db");
        let project_link = tmp.path().join("project.json");
        let mcp = AgentflareMcp::for_test(backend_db, repo_root, project_link);
        (tmp, mcp)
    }

    fn base_request() -> HandoffRequest {
        HandoffRequest {
            recipient: "claude-code".to_string(),
            name: "do the thing".to_string(),
            content: "content".to_string(),
            completed: "nothing yet".to_string(),
            remaining: "everything".to_string(),
            ..Default::default()
        }
    }

    fn item_label_names(mcp: &AgentflareMcp, item_id: &str) -> Vec<String> {
        mcp.with_backend_db(|conn| {
            let label_ids = agentflare_backend::item::list_labels(conn, item_id).unwrap();
            label_ids
                .iter()
                .map(|id| agentflare_backend::label::get(conn, id).unwrap().name)
                .collect()
        })
        .unwrap()
    }

    fn seed_ready_for_work_label(mcp: &AgentflareMcp) {
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            agentflare_backend::label::create(
                conn,
                agentflare_backend::label::CreateLabel {
                    project_id: Some(project.id),
                    workspace_id: project.workspace_id,
                    name: crate::supervisor::READY_LABEL.to_string(),
                    color: None,
                    parent_id: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap();
        })
        .unwrap();
    }

    #[test]
    fn new_item_gets_labeled_ready_for_work_when_the_project_has_that_label() {
        let (_tmp, mcp) = test_mcp();
        seed_ready_for_work_label(&mcp);

        let resp = mcp.handoff_impl(base_request()).unwrap();
        let item_id = serde_json::from_str::<serde_json::Value>(&resp).unwrap()["item_id"]
            .as_str()
            .unwrap()
            .to_string();

        assert!(
            item_label_names(&mcp, &item_id).contains(&crate::supervisor::READY_LABEL.to_string())
        );
    }

    #[test]
    fn new_item_is_not_labeled_when_the_project_has_no_ready_for_work_label() {
        let (_tmp, mcp) = test_mcp();
        // Deliberately not seeding the label — this project hasn't opted
        // into autonomous dispatch, so nothing should be created out of
        // nowhere and the handoff must still succeed.
        let resp = mcp.handoff_impl(base_request()).unwrap();
        let item_id = serde_json::from_str::<serde_json::Value>(&resp).unwrap()["item_id"]
            .as_str()
            .unwrap()
            .to_string();

        assert!(item_label_names(&mcp, &item_id).is_empty());
    }

    #[test]
    fn a_reply_to_an_existing_item_is_not_labeled_ready_for_work() {
        let (_tmp, mcp) = test_mcp();
        seed_ready_for_work_label(&mcp);

        // First handoff creates the item.
        let first = mcp.handoff_impl(base_request()).unwrap();
        let item_id = serde_json::from_str::<serde_json::Value>(&first).unwrap()["item_id"]
            .as_str()
            .unwrap()
            .to_string();
        // A human clears the label after picking it up manually, same as the
        // supervisor's own discovery tick would once it dispatches the item.
        mcp.with_backend_db(|conn| {
            let label_ids = agentflare_backend::item::list_labels(conn, &item_id).unwrap();
            for id in label_ids {
                agentflare_backend::item::remove_label(conn, &item_id, &id).unwrap();
            }
        })
        .unwrap();

        // A reply (item_id set) must not silently re-queue an item that may
        // already be claimed, in progress, or done.
        let reply = HandoffRequest {
            item_id: Some(item_id.clone()),
            completed: "more".to_string(),
            remaining: "less".to_string(),
            ..base_request()
        };
        mcp.handoff_impl(reply).unwrap();

        assert!(item_label_names(&mcp, &item_id).is_empty());
    }
}
