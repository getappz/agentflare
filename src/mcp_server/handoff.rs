use super::*;

/// The project's `ready-for-work` label id, if the project has that label at
/// all — skipped (returns `None`) rather than creating it out of nowhere.
/// Shared by both branches of `handoff_impl` below: a brand-new handed-off
/// item, and an existing item that's safe to queue (see the `Some(id)`
/// branch's own comment for what "safe" means there).
fn ready_label_id(conn: &rusqlite::Connection, project_id: &str) -> Option<String> {
    agentflare_backend::label::list_by_project(conn, project_id)
        .ok()?
        .into_iter()
        .find(|l| l.name == crate::supervisor::READY_LABEL)
        .map(|l| l.id)
}

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

        // "github" is reserved: it means "any workstation," not a specific
        // agent. Publishes as a labelled issue on the bridge's pull queue
        // instead of a local item -- the already-running bridge tick loop
        // (src/github/bridge/tick.rs) picks it up on whichever workstation
        // has claim headroom next, no local item/asset created here at all.
        if recipient.eq_ignore_ascii_case("github") {
            if item_id.is_some() {
                return Err(ErrorData::invalid_params(
                    "recipient=\"github\" publishes new work to the bridge queue -- it can't target an existing item_id",
                    None,
                ));
            }
            return self.handoff_to_bridge_queue(
                &name,
                &content,
                description.as_deref(),
                &completed,
                &remaining,
                thread_id.as_deref(),
            );
        }

        // Anything that isn't "github" and isn't a known agent type would
        // otherwise become `assignee_agent` on the created/updated item with
        // no validation at all -- a typo or a thematic word (e.g.
        // "gastown") silently orphans the item, since nothing will ever
        // claim or dispatch it. Same registry lookup `cli/work.rs`'s
        // `--agent` flag already validates against.
        if agent_registry::agent_by_name(&recipient).is_none() {
            let mut valid: Vec<&str> = agent_registry::REGISTRY
                .iter()
                .map(|s| s.id.as_str())
                .collect();
            valid.push("github");
            valid.sort_unstable();
            return Err(ErrorData::invalid_params(
                format!(
                    "recipient '{recipient}' is not a known agent -- valid recipients: {}",
                    valid.join(", ")
                ),
                None,
            ));
        }

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
                        .map(|item| flare_git_core::worktree::task_branch_name(&item))
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
                    let item = agentflare_backend::item::update(conn, id, input)
                        .map_err(map_backend_err)?;
                    // Queue it for autonomous dispatch too, same as the
                    // brand-new-item path below — but only when it's safe:
                    // genuinely fresh (backlog/unstarted/triage) and no live
                    // claim is currently held on it. An explicit `item_id`
                    // handoff onto something already claimed, in progress, in
                    // review, or completed must NOT be silently re-queued —
                    // same danger the reply/continuation branch below already
                    // guards against, just reached via a different path (a
                    // caller passing `item_id` directly instead of relying on
                    // name/thread matching). Without this, `handoff` onto an
                    // existing item only ever set the assignee — queuing it
                    // needed a separate `item add_label` call every time.
                    //
                    // Deliberately NOT `claim::current_owner` (item #82):
                    // that helper includes stale-but-undone claims by design
                    // ("so stale locks can be cleaned up" — its own doc
                    // comment), and the ledger row only ever leaves
                    // `status = 'claimed'` via `claim::done`, which only runs
                    // on the normal in_review -> completed promotion. An item
                    // whose work was interrupted or rescued outside that
                    // flow (crash, manual takeover) keeps a `claimed` row
                    // forever, so `current_owner` never goes `None` again —
                    // permanently vetoing re-labeling on every future handoff
                    // to that item, silently. A liveness check (TTL-aware,
                    // same primitive tier 4 of `quota::decide` and the
                    // bridge's own claim-liveness check already use) treats
                    // an expired claim as no claim, which is what "safe to
                    // re-queue" actually means here.
                    let state = agentflare_backend::state::get(conn, &item.state_id)
                        .map_err(map_backend_err)?;
                    let now = crate::claims::now();
                    let ttl_secs = crate::claims::ttl_secs();
                    let has_live_claim = agentflare_backend::claim::has_active_claim_by_other(
                        conn, id, "", now, ttl_secs,
                    )
                    // Fail closed: if the claim table can't be read, don't
                    // silently re-queue something that might be live.
                    .unwrap_or(true);
                    if !has_live_claim
                        && matches!(
                            state.group_name.as_str(),
                            "backlog" | "unstarted" | "triage"
                        )
                        && let Some(ready_id) = ready_label_id(conn, &project.id)
                    {
                        let _ = agentflare_backend::item::add_label(conn, id, &ready_id);
                    }
                    item
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
                        let ready_label_id = ready_label_id(conn, &project.id);
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

    /// Publishes `name`/body as a GitHub issue labelled with the bridge's
    /// queue label, on the repo resolved from `AGENTFLARE_BRIDGE_REPO`, else
    /// this repo's `.agentflare/config.toml` `[bridge].repo` override, else
    /// the workstation's `origin` remote (`bridge::config::resolve_project_repo`
    /// -- same chain `agentflare github-bridge` and `bridge_queue_status`
    /// resolve through). Deliberately thin: issue creation and the
    /// claim/heartbeat/export lifecycle already live in `github::issues` and
    /// `github::bridge::tick`; this just gets work onto the queue.
    ///
    /// Idempotent across retries while the previous attempt's issue is still
    /// UNCLAIMED: the full structured payload (`content`, `completed`,
    /// `remaining`, `thread_id`) and a dedup key (`thread_id`, else `name`)
    /// are embedded as a hidden marker in the issue body
    /// (`bridge::handoff_payload`) -- recovered by the bridge importer
    /// (`tick::record_claim`) when the issue is claimed, and looked up here
    /// first so a retry after a timeout reuses the existing issue instead of
    /// publishing a duplicate. Once claimed, a matching key is NOT reused --
    /// a local item already exists carrying that payload, and nothing
    /// re-reads the issue afterward, so reusing it would silently drop this
    /// call's (possibly updated) `completed`/`remaining` instead of
    /// publishing them as a fresh, distinct entry. The result's `reused`
    /// field says which happened.
    fn handoff_to_bridge_queue(
        &self,
        name: &str,
        content: &str,
        description: Option<&str>,
        completed: &str,
        remaining: &str,
        thread_id: Option<&str>,
    ) -> Result<String, ErrorData> {
        use crate::github::bridge::claim as claim_rules;
        use crate::github::bridge::handoff_payload::HandoffPayload;
        use crate::github::{Client, bridge::config, issues};

        let repo_root = self.worktree_repo_root();
        let repo = config::resolve_project_repo(&repo_root)
            .map_err(|e| ErrorData::invalid_params(e, None))?
            .ok_or_else(|| {
                ErrorData::invalid_params(
                    "recipient=\"github\" needs a GitHub `origin` remote in the current repo \
                     (or a [bridge] repo override in .agentflare/config.toml)",
                    None,
                )
            })?;
        let client = Client::new().map_err(to_mcp_error)?;
        let queue_label = config::resolve_project_queue_label(&repo_root);

        let key = thread_id.unwrap_or(name).to_string();
        let payload = HandoffPayload {
            key: key.clone(),
            content: content.to_string(),
            completed: completed.to_string(),
            remaining: remaining.to_string(),
            thread_id: thread_id.map(str::to_string),
        };

        // A bare retry after e.g. a network timeout must reuse the issue
        // this call already created rather than publish a second one --
        // `issues::create` has no idempotency of its own. But only while
        // that issue is still UNCLAIMED: once the bridge (or anything else)
        // has claimed it, a local item already exists carrying this exact
        // payload, and nothing re-reads the issue afterward -- returning it
        // again here would silently swallow this call's (possibly updated)
        // completed/remaining instead of the bare retry this exists for.
        // Same claim-liveness check `queue_status`/`tick` already use, so a
        // stale (expired) claim is correctly treated as no claim at all.
        let candidate = issues::list_filtered(&client, &repo, "open", Some(&queue_label), None)
            .map_err(to_mcp_error)?
            .into_iter()
            .find(|issue| {
                issue
                    .body
                    .as_deref()
                    .and_then(HandoffPayload::extract)
                    .is_some_and(|p| p.key == key)
            });
        let reusable = match candidate {
            Some(issue) => {
                let comments: Vec<(u64, String)> =
                    issues::list_comments(&client, &repo, issue.number, None)
                        .map_err(to_mcp_error)?
                        .into_iter()
                        .map(|c| (c.id, c.body))
                        .collect();
                let claimed = claim_rules::resolve_holder(
                    &comments,
                    crate::claims::now(),
                    crate::claims::ttl_secs(),
                )
                .is_some();
                (!claimed).then_some(issue)
            }
            None => None,
        };

        let (issue, reused) = match reusable {
            Some(issue) => (issue, true),
            None => {
                let body = payload.embed(description.unwrap_or(content));
                let issue = issues::create(
                    &client,
                    &repo,
                    name,
                    Some(&body),
                    std::slice::from_ref(&queue_label),
                    &[],
                )
                .map_err(to_mcp_error)?;
                (issue, false)
            }
        };

        let mut result = serde_json::json!({
            "repo": repo.to_string(),
            "issue_number": issue.number,
            "issue_url": issue.html_url,
            "queue_label": queue_label,
            "recipient": "github",
            "reused": reused,
        });

        // Report rather than reject: a project-local [bridge].repo override
        // can legitimately target a repo another workstation's daemon
        // watches, not this one -- see resolve_project_repo's module doc.
        // But if THIS workstation's daemon is enabled and points somewhere
        // else, say so -- nothing local will poll what was just published.
        if config::daemon_enabled()
            && let Some(daemon_repo) = config::resolve_daemon_repo(&repo_root)
            && daemon_repo != repo
        {
            result["warning"] = serde_json::json!(format!(
                "this workstation's bridge daemon is enabled but watches {daemon_repo} -- it \
                 will not poll {repo}; relying on another workstation's daemon to pick this up"
            ));
        }

        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
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
    fn recipient_github_rejects_an_item_id() {
        // Credential-independent, like flare_git_impl's own
        // unknown_action_is_rejected_before_repo_or_client_setup: this must
        // fail on its own merits before ever resolving a repo or a client.
        let (_tmp, mcp) = test_mcp();
        let req = HandoffRequest {
            recipient: "github".to_string(),
            item_id: Some("some-item".to_string()),
            ..base_request()
        };
        let err = mcp.handoff_impl(req).unwrap_err();
        assert!(err.to_string().contains("item_id"), "{err}");
    }

    #[test]
    fn recipient_github_without_an_origin_remote_fails_clearly() {
        // test_mcp()'s repo has no `origin` configured, so this exercises
        // handoff_to_bridge_queue's repo resolution without hitting the
        // network at all -- but only if AGENTFLARE_BRIDGE_REPO isn't
        // inherited from the outer environment; resolve_project_repo checks
        // it before `origin`, and a set value would let this reach
        // Client::new()/issues::create instead, hitting the network and
        // potentially creating a real issue. Cleared and restored under the
        // shared lock other env-mutating tests in this crate already use.
        let _guard = agent_registry::detect::PATH_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let prior = std::env::var("AGENTFLARE_BRIDGE_REPO").ok();
        unsafe {
            std::env::remove_var("AGENTFLARE_BRIDGE_REPO");
        }

        let (_tmp, mcp) = test_mcp();
        let req = HandoffRequest {
            recipient: "github".to_string(),
            ..base_request()
        };
        let err = mcp.handoff_impl(req).unwrap_err();
        assert!(err.to_string().contains("origin"), "{err}");

        unsafe {
            match &prior {
                Some(v) => std::env::set_var("AGENTFLARE_BRIDGE_REPO", v),
                None => std::env::remove_var("AGENTFLARE_BRIDGE_REPO"),
            }
        }
    }

    #[test]
    fn recipient_not_in_the_agent_registry_is_rejected_with_valid_recipients_listed() {
        let (_tmp, mcp) = test_mcp();
        let req = HandoffRequest {
            recipient: "gastown".to_string(),
            ..base_request()
        };
        let err = mcp.handoff_impl(req).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("gastown"), "{msg}");
        assert!(msg.contains("claude-code"), "{msg}");
        assert!(msg.contains("github"), "{msg}");
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
    fn a_reply_to_an_already_claimed_item_is_not_relabeled_ready_for_work() {
        let (_tmp, mcp) = test_mcp();
        seed_ready_for_work_label(&mcp);

        // First handoff creates the item.
        let first = mcp.handoff_impl(base_request()).unwrap();
        let item_id = serde_json::from_str::<serde_json::Value>(&first).unwrap()["item_id"]
            .as_str()
            .unwrap()
            .to_string();
        // The recipient actually claims it — moves to "started", creates a
        // claim row — same as the supervisor's own discovery tick dispatch
        // would once it picks the item up. This is the real condition the
        // explicit-`item_id` branch must protect against, not merely the
        // label being absent (see the sibling test below: a still-fresh
        // item with no label DOES get relabeled now, on purpose). Claims
        // directly against the backend (not through the MCP `item_claim`
        // wrapper, whose owner resolves ambiently from process identity —
        // not controllable here) with an owner matching the assignee
        // (`claude-code`), since `item::claim`'s handoff-freeze rule blocks
        // a mismatched owner from acquiring a freshly handed-off item.
        mcp.with_backend_db(|conn| {
            agentflare_backend::item::claim(
                conn,
                &item_id,
                "claude-code:test",
                db_kit::ids::now(),
                1800,
            )
        })
        .unwrap()
        .unwrap();
        // The real dispatch path (`supervisor::dispatch_item`) strips the
        // ready-for-work label the moment it enqueues a job, well before
        // the agent ever claims anything — mirror that here so this test
        // reflects the label state a real in-flight item actually has.
        mcp.with_backend_db(|conn| {
            let label_ids = agentflare_backend::item::list_labels(conn, &item_id).unwrap();
            for id in label_ids {
                agentflare_backend::item::remove_label(conn, &item_id, &id).unwrap();
            }
        })
        .unwrap();

        // A reply (item_id set) must not silently re-queue an item that's
        // already claimed, in progress, or done.
        let reply = HandoffRequest {
            item_id: Some(item_id.clone()),
            completed: "more".to_string(),
            remaining: "less".to_string(),
            ..base_request()
        };
        mcp.handoff_impl(reply).unwrap();

        assert!(item_label_names(&mcp, &item_id).is_empty());
    }

    #[test]
    fn an_explicit_item_id_handoff_labels_a_still_fresh_unclaimed_item() {
        let (_tmp, mcp) = test_mcp();
        seed_ready_for_work_label(&mcp);

        // First handoff creates the item, then a human clears the label
        // without actually claiming it (e.g. picked up by hand outside the
        // autonomous queue, or just never got labeled the first time).
        let first = mcp.handoff_impl(base_request()).unwrap();
        let item_id = serde_json::from_str::<serde_json::Value>(&first).unwrap()["item_id"]
            .as_str()
            .unwrap()
            .to_string();
        mcp.with_backend_db(|conn| {
            let label_ids = agentflare_backend::item::list_labels(conn, &item_id).unwrap();
            for id in label_ids {
                agentflare_backend::item::remove_label(conn, &item_id, &id).unwrap();
            }
        })
        .unwrap();

        // A second handoff onto the same still-fresh, never-claimed item
        // must queue it for dispatch — the whole point of this change is
        // that a single `handoff(item_id=...)` call is enough, no separate
        // `item add_label` call required.
        let reply = HandoffRequest {
            item_id: Some(item_id.clone()),
            completed: "more".to_string(),
            remaining: "less".to_string(),
            ..base_request()
        };
        mcp.handoff_impl(reply).unwrap();

        assert!(
            item_label_names(&mcp, &item_id).contains(&crate::supervisor::READY_LABEL.to_string())
        );
    }

    #[test]
    fn an_explicit_item_id_handoff_labels_an_item_with_only_a_stale_claim() {
        // item #82: a claim row only ever leaves `status = 'claimed'` via
        // `claim::done`, which only runs on the normal in_review ->
        // completed promotion. Work interrupted or rescued outside that
        // flow (crash, manual takeover) leaves the row `claimed` forever —
        // `claim::current_owner` (deliberately stale-inclusive, for cleanup
        // tooling) would see that and permanently refuse to relabel this
        // item on every future handoff, even though the item itself is
        // still sitting fresh in a backlog-group state and nothing live is
        // touching it. A liveness-aware check must treat the expired claim
        // as no claim.
        let (_tmp, mcp) = test_mcp();
        seed_ready_for_work_label(&mcp);

        let first = mcp.handoff_impl(base_request()).unwrap();
        let item_id = serde_json::from_str::<serde_json::Value>(&first).unwrap()["item_id"]
            .as_str()
            .unwrap()
            .to_string();
        mcp.with_backend_db(|conn| {
            let label_ids = agentflare_backend::item::list_labels(conn, &item_id).unwrap();
            for id in label_ids {
                agentflare_backend::item::remove_label(conn, &item_id, &id).unwrap();
            }
        })
        .unwrap();

        // A stale claim, acquired directly against the ledger (not via
        // `item::claim`, which would also flip the item's state to
        // "started" -- this reproduces an item that's still state-wise
        // fresh but carries an abandoned claim row) with a heartbeat far
        // enough in the past to be expired under any real TTL.
        mcp.with_backend_db(|conn| {
            agentflare_backend::claim::acquire(conn, &item_id, "claude-code:ghost", 1, 1800)
        })
        .unwrap()
        .unwrap();

        let reply = HandoffRequest {
            item_id: Some(item_id.clone()),
            completed: "more".to_string(),
            remaining: "less".to_string(),
            ..base_request()
        };
        mcp.handoff_impl(reply).unwrap();

        assert!(
            item_label_names(&mcp, &item_id).contains(&crate::supervisor::READY_LABEL.to_string()),
            "a stale, never-`done`/`released` claim must not permanently block re-labeling"
        );
    }
}
