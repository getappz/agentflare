//! `item` MCP tool action handlers — split out of `mcp_server.rs`'s
//! `item_inner` dispatcher (was a single 411-line function, the file's
//! largest and its top complexity hotspot). Each `fn item_<action>` here is
//! the exact body that used to live in `item_inner`'s matching arm, moved
//! verbatim; `item_inner` itself is now just the `match` dispatch.

use super::*;

impl AgentflareMcp {
    pub(super) fn item_create(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let name = req
            .name
            .ok_or_else(|| ErrorData::invalid_params("name is required for create", None))?;
        if name.trim().is_empty() {
            return Err(ErrorData::invalid_params("name is required", None));
        }
        self.with_backend_db(|conn| {
            let project = self.resolve_project(conn)?;
            let state_id = match req.state_id {
                Some(s) => s,
                None => {
                    agentflare_backend::state::list_by_project(conn, &project.id)
                        .map_err(map_backend_err)?
                        .into_iter()
                        .find(|s| s.is_default)
                        .ok_or_else(|| {
                            ErrorData::internal_error("project has no default state", None)
                        })?
                        .id
                }
            };
            let input = agentflare_backend::item::CreateItem {
                project_id: project.id,
                state_id,
                name,
                description: req.description,
                priority: req.priority,
                parent_id: req.parent_id,
                assignee_agent: req.assignee_agent,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: req.metadata.map(|v| v.to_string()),
                label_ids: req.label_ids.unwrap_or_default(),
                assignee_ids: vec![],
                dependency_ids: req.dependency_ids.unwrap_or_default(),
            };
            let item = agentflare_backend::item::create(conn, input).map_err(map_backend_err)?;
            Ok(serde_json::to_string_pretty(&item).unwrap_or_default())
        })?
    }

    pub(super) fn item_get(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let id = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for get", None))?;
        if id.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        self.with_backend_db(|conn| {
            let item = agentflare_backend::item::get(conn, &id).map_err(map_backend_err)?;
            Ok(serde_json::to_string_pretty(&item).unwrap_or_default())
        })?
    }

    pub(super) fn item_list(&self, req: ItemRequest) -> Result<String, ErrorData> {
        if req.limit.is_some_and(|l| l < 0) || req.offset.is_some_and(|o| o < 0) {
            return Err(ErrorData::invalid_params(
                "limit and offset must be non-negative",
                None,
            ));
        }
        self.with_backend_db(|conn| {
            let project = self.resolve_project(conn)?;
            let mut items = agentflare_backend::item::list_by_project(conn, &project.id)
                .map_err(map_backend_err)?;
            let states = agentflare_backend::state::list_by_project(conn, &project.id)
                .map_err(map_backend_err)?;
            let state_by_id: std::collections::HashMap<&str, &agentflare_backend::state::State> =
                states.iter().map(|s| (s.id.as_str(), s)).collect();

            if let Some(group) = &req.state_group {
                let wanted: Vec<&str> = group.split(',').map(str::trim).collect();
                items.retain(|i| {
                    state_by_id
                        .get(i.state_id.as_str())
                        .map(|s| wanted.contains(&s.group_name.as_str()))
                        .unwrap_or(false)
                });
            }
            if let Some(agent) = &req.assignee_agent {
                items.retain(|i| {
                    i.assignee_agent.as_deref() == Some(agent.as_str())
                        || i.assignee_agent.is_none()
                });
                items.sort_by_key(|i| {
                    let is_open = state_by_id
                        .get(i.state_id.as_str())
                        .map(|s| !matches!(s.group_name.as_str(), "completed" | "cancelled"))
                        .unwrap_or(true);
                    let is_mine = i.assignee_agent.as_deref() == Some(agent.as_str());
                    (!is_open, !is_mine)
                });
            }

            let offset = req.offset.unwrap_or(0) as usize;
            let items = items.into_iter().skip(offset);
            let items: Vec<_> = match req.limit {
                Some(limit) => items.take(limit as usize).collect(),
                None => items.collect(),
            };

            let summaries: Vec<ItemSummary> = items
                .into_iter()
                .map(|i| {
                    let state = state_by_id.get(i.state_id.as_str());
                    ItemSummary {
                        id: i.id,
                        name: i.name,
                        state: state.map(|s| s.name.clone()).unwrap_or_default(),
                        state_group: state.map(|s| s.group_name.clone()).unwrap_or_default(),
                        priority: i.priority,
                        assignee_agent: i.assignee_agent,
                        parent_id: i.parent_id,
                        sequence_id: i.sequence_id,
                        updated_at: i.updated_at,
                    }
                })
                .collect();
            Ok(serde_json::to_string_pretty(&summaries).unwrap_or_default())
        })?
    }

    pub(super) fn item_update(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let id = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for update", None))?;
        if id.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        self.with_backend_db(|conn| {
            let input = agentflare_backend::item::UpdateItem {
                name: req.name,
                description: req.description,
                priority: req.priority,
                state_id: None,
                assignee_agent: req.assignee_agent,
                sort_order: None,
            };
            let item =
                agentflare_backend::item::update(conn, &id, input).map_err(map_backend_err)?;
            Ok(serde_json::to_string_pretty(&item).unwrap_or_default())
        })?
    }

    pub(super) fn item_update_state(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let id = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for update_state", None))?;
        let state_id = req.state_id.ok_or_else(|| {
            ErrorData::invalid_params("state_id is required for update_state", None)
        })?;
        if id.trim().is_empty() || state_id.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "id and state_id are required",
                None,
            ));
        }
        self.with_backend_db(|conn| {
            let item = agentflare_backend::item::update_state(conn, &id, &state_id)
                .map_err(map_backend_err)?;
            Ok(serde_json::to_string_pretty(&item).unwrap_or_default())
        })?
    }

    pub(super) fn item_delete(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let id = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for delete", None))?;
        if id.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        self.with_backend_db(|conn| {
            agentflare_backend::item::delete(conn, &id).map_err(map_backend_err)?;
            Ok(serde_json::json!({"deleted": true, "id": id}).to_string())
        })?
    }

    pub(super) fn item_claim(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let item_id = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for claim", None))?;
        if item_id.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let owner = crate::claims::owner_id();
        let now = crate::claims::now();
        let ttl = backend_claim_ttl_secs();
        let repo_root = self.worktree_repo_root();
        // Only resolve the item + target branch (DB reads) under the
        // backend lock; `git worktree add` below is a blocking
        // filesystem+subprocess operation that has no business
        // running while the shared DB mutex is held.
        let (outcome, item, target_branch) = self.with_backend_db(|conn| {
            let outcome = agentflare_backend::item::claim(conn, &item_id, &owner, now, ttl)
                .map_err(map_backend_err)?;
            let (item, target_branch) = if outcome == agentflare_backend::claim::Acquire::Acquired {
                let item = agentflare_backend::item::get(conn, &item_id).ok();
                let target_branch = item
                    .as_ref()
                    .map(|i| crate::worktree::resolve_target_branch(conn, i, &repo_root));
                (item, target_branch)
            } else {
                (None, None)
            };
            Ok::<_, ErrorData>((outcome, item, target_branch))
        })??;
        let worktree_path = match (&item, &target_branch) {
            (Some(item), Some(target)) => PROGRESS_SENDER
                .try_with(|ps| {
                    crate::worktree::create_worktree(item, &repo_root, target, ps.as_ref())
                })
                .unwrap_or_else(|_| {
                    crate::worktree::create_worktree(item, &repo_root, target, None)
                }),
            _ => None,
        };
        Ok(match outcome {
            agentflare_backend::claim::Acquire::Acquired => {
                let mut resp = serde_json::json!({
                    "status": "acquired",
                    "item_id": item_id,
                    "owner": owner,
                });
                if let Some(ref path) = worktree_path {
                    resp["worktree_path"] =
                        serde_json::Value::String(path.to_string_lossy().to_string());
                }
                resp.to_string()
            }
            agentflare_backend::claim::Acquire::Held {
                owner: holder,
                age_secs,
            } => serde_json::json!({"status": "held", "item_id": item_id, "owner": holder, "age_secs": age_secs}).to_string(),
        })
    }

    pub(super) fn item_heartbeat(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let item_id = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for heartbeat", None))?;
        if item_id.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let owner = crate::claims::owner_id();
        let now = crate::claims::now();
        self.with_backend_db(|conn| {
            let ok = agentflare_backend::claim::heartbeat(conn, &item_id, &owner, now)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            Ok(serde_json::json!({"heartbeat": ok, "item_id": item_id}).to_string())
        })?
    }

    pub(super) fn item_release(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let item_id = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for release", None))?;
        if item_id.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let owner = crate::claims::owner_id();
        self.with_backend_db(|conn| {
            let ok = agentflare_backend::claim::release(conn, &item_id, &owner)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            Ok(serde_json::json!({"released": ok, "item_id": item_id}).to_string())
        })?
    }

    pub(super) fn item_done(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let item_id = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for done", None))?;
        if item_id.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let owner = crate::claims::owner_id();
        let now = crate::claims::now();
        let repo_root = self.worktree_repo_root();
        // Same split as `claim`: resolve (DB reads) under the
        // backend lock, then run the blocking git/gh push+PR outside
        // it — `git push`/`gh pr create` have no business running
        // while the shared DB mutex is held.
        let (done, item, target_branch) = self.with_backend_db(|conn| {
            let done = agentflare_backend::item::mark_completed(conn, &item_id, &owner)
                .map_err(map_backend_err)?;
            let (item, target_branch) = if done {
                // Refresh the lease's heartbeat right before the
                // potentially long push/PR publish step below, so a
                // short custom AGENTFLARE_BACKEND_CLAIM_TTL_SECS
                // can't let it go stale mid-flight (item #37
                // follow-up).
                let _ = agentflare_backend::claim::heartbeat(conn, &item_id, &owner, now);
                let item = agentflare_backend::item::get(conn, &item_id).ok();
                let target_branch = item
                    .as_ref()
                    .map(|i| crate::worktree::resolve_target_branch(conn, i, &repo_root));
                (item, target_branch)
            } else {
                (None, None)
            };
            Ok::<_, ErrorData>((done, item, target_branch))
        })??;
        let pr_url = match (&item, &target_branch) {
            (Some(item), Some(target)) => PROGRESS_SENDER
                .try_with(|ps| {
                    crate::worktree::push_and_open_pr(item, &repo_root, target, ps.as_ref())
                })
                .unwrap_or_else(|_| {
                    crate::worktree::push_and_open_pr(item, &repo_root, target, None)
                }),
            _ => None,
        };
        // Only now — after push_and_open_pr has been attempted
        // (success or soft-fail, it never blocks) — actually release
        // the claim lease. Keeping it held until this point closes
        // the race where a concurrent claim() could grab the item
        // while its PR was still being opened (item #37).
        if done {
            match self.with_backend_db(|conn| {
                agentflare_backend::claim::done(conn, &item_id, &owner, now)
            }) {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => eprintln!(
                    "worktree: releasing claim for item {item_id} affected no rows (owner mismatch or already released)"
                ),
                Ok(Err(e)) => {
                    eprintln!("worktree: failed to release claim for item {item_id}: {e}")
                }
                Err(e) => {
                    eprintln!("worktree: failed to release claim for item {item_id}: {e:?}")
                }
            }
        }
        let mut resp = serde_json::json!({"done": done, "item_id": item_id});
        if let Some(url) = pr_url {
            resp["pr_url"] = serde_json::Value::String(url.clone());
        }
        Ok(resp.to_string())
    }

    pub(super) fn item_cancel(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let item_id = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for cancel", None))?;
        if item_id.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let owner = crate::claims::owner_id();
        self.with_backend_db(|conn| {
            let project = self.resolve_project(conn)?;
            let cancelled =
                agentflare_backend::state::first_in_group(conn, &project.id, "cancelled")
                    .map_err(map_backend_err)?;
            let item = agentflare_backend::item::update_state(conn, &item_id, &cancelled.id)
                .map_err(map_backend_err)?;
            // Best-effort: release this caller's own claim lease on
            // the item, if any, so a cancelled item isn't stuck
            // "held" until the TTL expires (mirrors `done`'s
            // claim_done release). No-ops if someone else holds it
            // or nobody does — `release` is owner-scoped.
            let _ = agentflare_backend::claim::release(conn, &item_id, &owner);
            Ok(serde_json::to_string_pretty(&item).unwrap_or_default())
        })?
    }

    pub(super) fn item_search(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let query = req
            .query
            .ok_or_else(|| ErrorData::invalid_params("query is required for search", None))?;
        if query.trim().is_empty() {
            return Err(ErrorData::invalid_params("query is required", None));
        }
        self.with_backend_db(|conn| {
            let project = self.resolve_project(conn)?;
            let items = agentflare_backend::item::search(
                conn,
                &project.id,
                &query,
                req.limit.map(|l| l as usize),
            )
            .map_err(map_backend_err)?;
            Ok(serde_json::to_string_pretty(&items).unwrap_or_default())
        })?
    }

    pub(super) fn item_add_label(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let item_id = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for add_label", None))?;
        let label_id = req
            .label_id
            .ok_or_else(|| ErrorData::invalid_params("label_id is required for add_label", None))?;
        if item_id.trim().is_empty() || label_id.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "id and label_id are required",
                None,
            ));
        }
        self.with_backend_db(|conn| {
            agentflare_backend::item::add_label(conn, &item_id, &label_id)
                .map_err(map_backend_err)?;
            Ok(
                serde_json::json!({"attached": true, "item_id": item_id, "label_id": label_id})
                    .to_string(),
            )
        })?
    }

    pub(super) fn item_remove_label(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let item_id = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for remove_label", None))?;
        let label_id = req.label_id.ok_or_else(|| {
            ErrorData::invalid_params("label_id is required for remove_label", None)
        })?;
        if item_id.trim().is_empty() || label_id.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "id and label_id are required",
                None,
            ));
        }
        self.with_backend_db(|conn| {
            agentflare_backend::item::remove_label(conn, &item_id, &label_id)
                .map_err(map_backend_err)?;
            Ok(
                serde_json::json!({"removed": true, "item_id": item_id, "label_id": label_id})
                    .to_string(),
            )
        })?
    }
}
