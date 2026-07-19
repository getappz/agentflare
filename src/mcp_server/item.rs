//! `item` MCP tool action handlers — split out of `mcp_server.rs`'s
//! `item_inner` dispatcher (was a single 411-line function, the file's
//! largest and its top complexity hotspot). Each `fn item_<action>` here is
//! the exact body that used to live in `item_inner`'s matching arm, moved
//! verbatim; `item_inner` itself is now just the `match` dispatch.

use super::*;
use rusqlite::Connection;

/// Bounds `groom`'s shortlist size — caps the O(n^2) duplicate-detection
/// pass and the SQLite `IN (...)` parameter list built from it.
const MAX_GROOM_LIMIT: i64 = 200;

/// Bounds `health`'s velocity window — without this, a caller-supplied
/// `window_weeks` (e.g. `i64::MAX`) would build a `Vec<VelocityWeek>` of
/// that literal size regardless of how much data actually exists, while
/// holding the backend DB lock.
const MAX_WINDOW_WEEKS: i64 = 52;

fn priority_rank(p: &str) -> u8 {
    match p {
        "urgent" => 5,
        "high" => 4,
        "medium" => 3,
        "low" => 2,
        _ => 1,
    }
}

/// `size` lives in the free-form `metadata` JSON blob (`{"size": "S"|"M"|"L"}`)
/// rather than a regex over description prose — sets via `item(update)`.
fn parsed_size(metadata: &str) -> Option<String> {
    let mut value = serde_json::from_str::<serde_json::Value>(metadata).ok()?;
    // Defensive: some callers double-encode an object-typed param as a JSON
    // string containing JSON (observed live — item(create) with
    // metadata={"size":"S"} stored `"{\"size\": \"S\"}"` instead of the
    // object). Unwrap one extra layer before giving up.
    if let serde_json::Value::String(inner) = &value
        && let Ok(reparsed) = serde_json::from_str::<serde_json::Value>(inner)
    {
        value = reparsed;
    }
    value
        .get("size")?
        .as_str()
        .filter(|s| matches!(*s, "S" | "M" | "L"))
        .map(str::to_string)
}

/// Open-dependency blocking per item, from edges that already carry the
/// dependency target's true state_group (joined server-side in
/// `dependency_edges_for_items` — so blocking status is correct even when
/// the target isn't in the same shortlist/limit window as the item).
fn blocked_by_map(
    edges: &[(String, String, String)],
) -> std::collections::HashMap<String, Vec<String>> {
    let mut blocked_by: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for (item_id, depends_on, target_group) in edges {
        if !matches!(target_group.as_str(), "completed" | "cancelled") {
            blocked_by
                .entry(item_id.clone())
                .or_default()
                .push(depends_on.clone());
        }
    }
    blocked_by
}

/// Near-duplicate names within a shortlist (token-Jaccard ≥ 0.5) — no
/// embeddings needed at this backlog scale.
fn near_duplicates(
    shortlist: &[agentflare_backend::item::Item],
) -> std::collections::HashMap<String, Vec<String>> {
    fn name_tokens(name: &str) -> std::collections::HashSet<String> {
        name.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|s| s.len() > 2)
            .map(str::to_string)
            .collect()
    }
    let token_sets: Vec<_> = shortlist.iter().map(|i| name_tokens(&i.name)).collect();
    let mut duplicates: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    for a in 0..shortlist.len() {
        for b in (a + 1)..shortlist.len() {
            let (sa, sb) = (&token_sets[a], &token_sets[b]);
            if sa.is_empty() || sb.is_empty() {
                continue;
            }
            let inter = sa.intersection(sb).count() as f64;
            let union = sa.union(sb).count() as f64;
            if union > 0.0 && inter / union >= 0.5 {
                duplicates
                    .entry(shortlist[a].id.clone())
                    .or_default()
                    .push(shortlist[b].id.clone());
                duplicates
                    .entry(shortlist[b].id.clone())
                    .or_default()
                    .push(shortlist[a].id.clone());
            }
        }
    }
    duplicates
}

fn to_standup_item(i: &agentflare_backend::item::Item) -> StandupItem {
    StandupItem {
        id: i.id.clone(),
        sequence_id: i.sequence_id,
        name: i.name.clone(),
        priority: i.priority.clone(),
        assignee_agent: i.assignee_agent.clone(),
        updated_at: i.updated_at,
    }
}

/// Now/Next/Later planning buckets. Unestimated items are excluded outright
/// (can't be planned without a size); of the rest, blocked items go to
/// `later`, and ready items split into `now` (first `capacity`, in existing
/// rank order) and `next` (the remainder).
fn capacity_buckets(
    items: &[GroomItem],
    capacity: i64,
) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let capacity = capacity.max(0) as usize;
    let mut needs_estimation = Vec::new();
    let mut later = Vec::new();
    let mut ready = Vec::new();
    for i in items {
        if i.unestimated {
            needs_estimation.push(i.id.clone());
        } else if !i.blocked_by.is_empty() {
            later.push(i.id.clone());
        } else {
            ready.push(i.id.clone());
        }
    }
    let next = ready.split_off(capacity.min(ready.len()));
    (ready, next, later, needs_estimation)
}

impl AgentflareMcp {
    /// Resolve a user-supplied id to an item UUID.
    /// Accepts a UUID (pass-through) or a numeric `sequence_id`.
    pub(crate) fn resolve_item_id(
        &self,
        conn: &Connection,
        id_or_seq: &str,
    ) -> Result<String, ErrorData> {
        let project = self.resolve_project(conn)?;
        agentflare_backend::item::resolve_id(conn, Some(&project.id), id_or_seq)
            .map_err(map_backend_err)
    }

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
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for get", None))?;
        if raw.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        self.with_backend_db(|conn| {
            let id = self.resolve_item_id(conn, &raw)?;
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
            // #75: default the filter to the server-derived identity when the
            // caller omits it, so a bare `item(list)` behaves like an inbox
            // (mine + unassigned) instead of dumping every item. An explicit
            // value is still honored — this is a read-only visibility filter,
            // not an authorization boundary, so viewing a teammate's queue is
            // allowed. Falls back to no filter only when identity is undetected.
            let assignee = req.assignee_agent.clone().or_else(|| self.agent.clone());
            if let Some(agent) = &assignee {
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
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for update", None))?;
        if raw.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        self.with_backend_db(|conn| {
            let id = self.resolve_item_id(conn, &raw)?;
            let input = agentflare_backend::item::UpdateItem {
                name: req.name,
                description: req.description,
                priority: req.priority,
                state_id: None,
                assignee_agent: req.assignee_agent,
                sort_order: None,
                metadata: req.metadata.map(|v| v.to_string()),
            };
            let item =
                agentflare_backend::item::update(conn, &id, input).map_err(map_backend_err)?;
            Ok(serde_json::to_string_pretty(&item).unwrap_or_default())
        })?
    }

    pub(super) fn item_update_state(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for update_state", None))?;
        let state_id = req.state_id.ok_or_else(|| {
            ErrorData::invalid_params("state_id is required for update_state", None)
        })?;
        if raw.trim().is_empty() || state_id.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "id and state_id are required",
                None,
            ));
        }
        self.with_backend_db(|conn| {
            let id = self.resolve_item_id(conn, &raw)?;
            let item = agentflare_backend::item::update_state(conn, &id, &state_id)
                .map_err(map_backend_err)?;
            Ok(serde_json::to_string_pretty(&item).unwrap_or_default())
        })?
    }

    pub(super) fn item_delete(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for delete", None))?;
        if raw.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        self.with_backend_db(|conn| {
            let id = self.resolve_item_id(conn, &raw)?;
            agentflare_backend::item::delete(conn, &id).map_err(map_backend_err)?;
            Ok(serde_json::json!({"deleted": true, "id": id}).to_string())
        })?
    }

    pub(crate) fn item_claim(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for claim", None))?;
        if raw.trim().is_empty() {
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
        let (outcome, item_id, item, target_branch) = self.with_backend_db(|conn| {
            let item_id = self.resolve_item_id(conn, &raw)?;
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
            Ok::<_, ErrorData>((outcome, item_id, item, target_branch))
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
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for heartbeat", None))?;
        if raw.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let owner = crate::claims::owner_id();
        let now = crate::claims::now();
        self.with_backend_db(|conn| {
            let item_id = self.resolve_item_id(conn, &raw)?;
            let ok = agentflare_backend::claim::heartbeat(conn, &item_id, &owner, now)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            Ok(serde_json::json!({"heartbeat": ok, "item_id": item_id}).to_string())
        })?
    }

    pub(crate) fn item_release(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for release", None))?;
        if raw.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let owner = crate::claims::owner_id();
        self.with_backend_db(|conn| {
            let item_id = self.resolve_item_id(conn, &raw)?;
            let ok = agentflare_backend::claim::release(conn, &item_id, &owner)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            Ok(serde_json::json!({"released": ok, "item_id": item_id}).to_string())
        })?
    }

    pub(crate) fn item_done(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for done", None))?;
        if raw.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let owner = crate::claims::owner_id();
        let now = crate::claims::now();
        let repo_root = self.worktree_repo_root();
        // Same split as `claim`: resolve (DB reads) under the
        // backend lock, then run the blocking git/gh push+PR outside
        // it — `git push`/`gh pr create` have no business running
        // while the shared DB mutex is held.
        let (done, item_id, item, target_branch) = self.with_backend_db(|conn| {
            let item_id = self.resolve_item_id(conn, &raw)?;
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
            Ok::<_, ErrorData>((done, item_id, item, target_branch))
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
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for cancel", None))?;
        if raw.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let owner = crate::claims::owner_id();
        self.with_backend_db(|conn| {
            let item_id = self.resolve_item_id(conn, &raw)?;
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
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for add_label", None))?;
        let label_id = req
            .label_id
            .ok_or_else(|| ErrorData::invalid_params("label_id is required for add_label", None))?;
        if raw.trim().is_empty() || label_id.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "id and label_id are required",
                None,
            ));
        }
        self.with_backend_db(|conn| {
            let item_id = self.resolve_item_id(conn, &raw)?;
            agentflare_backend::item::add_label(conn, &item_id, &label_id)
                .map_err(map_backend_err)?;
            Ok(
                serde_json::json!({"attached": true, "item_id": item_id, "label_id": label_id})
                    .to_string(),
            )
        })?
    }

    pub(super) fn item_remove_label(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for remove_label", None))?;
        let label_id = req.label_id.ok_or_else(|| {
            ErrorData::invalid_params("label_id is required for remove_label", None)
        })?;
        if raw.trim().is_empty() || label_id.trim().is_empty() {
            return Err(ErrorData::invalid_params(
                "id and label_id are required",
                None,
            ));
        }
        self.with_backend_db(|conn| {
            let item_id = self.resolve_item_id(conn, &raw)?;
            agentflare_backend::item::remove_label(conn, &item_id, &label_id)
                .map_err(map_backend_err)?;
            Ok(
                serde_json::json!({"removed": true, "item_id": item_id, "label_id": label_id})
                    .to_string(),
            )
        })?
    }

    /// One-call groom: filtered + priority/staleness-ranked shortlist with
    /// full description plus stale/unassigned/blocked/duplicate signals
    /// computed server-side. Replaces the `list` + N×`get` round trips a
    /// manual groom otherwise costs.
    pub(super) fn item_groom(&self, req: ItemRequest) -> Result<String, ErrorData> {
        if req.limit.is_some_and(|l| l < 0) {
            return Err(ErrorData::invalid_params(
                "limit must be non-negative",
                None,
            ));
        }
        let staleness_days = req.staleness_days.unwrap_or(14).max(0);
        // Bounds the shortlist's O(n^2) duplicate-detection pass and the
        // SQLite `IN (...)` parameter list built from it.
        let cap = req.limit.unwrap_or(15).clamp(0, MAX_GROOM_LIMIT) as usize;
        self.with_backend_db(|conn| {
            let project = self.resolve_project(conn)?;
            let mut items = agentflare_backend::item::list_by_project(conn, &project.id)
                .map_err(map_backend_err)?;
            let states = agentflare_backend::state::list_by_project(conn, &project.id)
                .map_err(map_backend_err)?;
            let state_by_id: std::collections::HashMap<&str, &agentflare_backend::state::State> =
                states.iter().map(|s| (s.id.as_str(), s)).collect();

            let wanted_groups: Vec<&str> = req
                .state_group
                .as_deref()
                .unwrap_or("backlog,unstarted")
                .split(',')
                .map(str::trim)
                .collect();
            items.retain(|i| {
                state_by_id
                    .get(i.state_id.as_str())
                    .map(|s| wanted_groups.contains(&s.group_name.as_str()))
                    .unwrap_or(false)
            });

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let stale_cutoff = now - staleness_days.saturating_mul(86_400);

            // Priority first, then most-recently-touched within a priority tier.
            items.sort_by(|a, b| {
                priority_rank(&b.priority)
                    .cmp(&priority_rank(&a.priority))
                    .then(b.updated_at.cmp(&a.updated_at))
            });
            let shortlist: Vec<_> = items.into_iter().take(cap).collect();

            let ids: Vec<String> = shortlist.iter().map(|i| i.id.clone()).collect();
            let edges = agentflare_backend::item::dependency_edges_for_items(conn, &ids)
                .map_err(map_backend_err)?;
            let blocked_by = blocked_by_map(&edges);
            let fanin = agentflare_backend::item::dependency_fanin_for_items(conn, &ids)
                .map_err(map_backend_err)?;
            let duplicates = near_duplicates(&shortlist);

            let groom_items: Vec<GroomItem> = shortlist
                .into_iter()
                .map(|i| {
                    let state = state_by_id.get(i.state_id.as_str());
                    let stale = i.updated_at < stale_cutoff;
                    let unassigned = i.assignee_agent.is_none();
                    let size = parsed_size(&i.metadata);
                    let unestimated = size.is_none();
                    GroomItem {
                        blocked_by: blocked_by.get(&i.id).cloned().unwrap_or_default(),
                        depended_on_by_count: *fanin.get(&i.id).unwrap_or(&0),
                        possible_duplicates: duplicates.get(&i.id).cloned().unwrap_or_default(),
                        id: i.id,
                        sequence_id: i.sequence_id,
                        name: i.name,
                        description: i.description,
                        state: state.map(|s| s.name.clone()).unwrap_or_default(),
                        state_group: state.map(|s| s.group_name.clone()).unwrap_or_default(),
                        priority: i.priority,
                        assignee_agent: i.assignee_agent,
                        updated_at: i.updated_at,
                        stale,
                        unassigned,
                        size,
                        unestimated,
                    }
                })
                .collect();

            let pull_next: Vec<String> = groom_items
                .iter()
                .filter(|i| i.unassigned && !i.stale && i.blocked_by.is_empty())
                .take(3)
                .map(|i| i.id.clone())
                .collect();

            // Only computed when `capacity` is set — omitted from the response
            // otherwise (backward compatible).
            let (now, next, later, needs_estimation) = match req.capacity {
                Some(capacity) => {
                    let (now, next, later, needs_estimation) =
                        capacity_buckets(&groom_items, capacity);
                    (Some(now), Some(next), Some(later), Some(needs_estimation))
                }
                None => (None, None, None, None),
            };

            let resp = GroomResponse {
                staleness_days,
                stale_count: groom_items.iter().filter(|i| i.stale).count(),
                unassigned_count: groom_items.iter().filter(|i| i.unassigned).count(),
                unestimated_count: groom_items.iter().filter(|i| i.unestimated).count(),
                items: groom_items,
                pull_next,
                now,
                next,
                later,
                needs_estimation,
            };
            Ok(serde_json::to_string_pretty(&resp).unwrap_or_default())
        })?
    }

    /// One-call standup: done/in-progress(grouped by assignee)/stuck, computed
    /// server-side from a single state-filtered read instead of the caller
    /// bucketing a flat `list` result by hand.
    pub(super) fn item_standup(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let cutoff_hours = req.cutoff_hours.unwrap_or(24).max(0);
        let stuck_days = req.staleness_days.unwrap_or(7).max(0);
        self.with_backend_db(|conn| {
            let project = self.resolve_project(conn)?;
            let mut items = agentflare_backend::item::list_by_project(conn, &project.id)
                .map_err(map_backend_err)?;
            let states = agentflare_backend::state::list_by_project(conn, &project.id)
                .map_err(map_backend_err)?;
            let state_by_id: std::collections::HashMap<&str, &agentflare_backend::state::State> =
                states.iter().map(|s| (s.id.as_str(), s)).collect();
            items.retain(|i| {
                state_by_id
                    .get(i.state_id.as_str())
                    .map(|s| matches!(s.group_name.as_str(), "started" | "completed"))
                    .unwrap_or(false)
            });

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let done_cutoff = now - cutoff_hours.saturating_mul(3_600);
            let stuck_cutoff = now - stuck_days.saturating_mul(86_400);

            // completed_at, not updated_at: editing an already-completed item
            // (e.g. fixing a typo) bumps updated_at without re-completing it —
            // using updated_at here would make old work spuriously reappear
            // in a "done recently" digest.
            let mut done_items: Vec<&agentflare_backend::item::Item> = items
                .iter()
                .filter(|i| {
                    state_by_id
                        .get(i.state_id.as_str())
                        .map(|s| s.group_name == "completed")
                        .unwrap_or(false)
                        && i.completed_at.is_some_and(|t| t >= done_cutoff)
                })
                .collect();
            done_items.sort_by_key(|i| std::cmp::Reverse(i.completed_at));
            let done: Vec<StandupItem> = done_items.into_iter().map(to_standup_item).collect();

            let in_progress_items: Vec<_> = items
                .iter()
                .filter(|i| {
                    state_by_id
                        .get(i.state_id.as_str())
                        .map(|s| s.group_name == "started")
                        .unwrap_or(false)
                })
                .collect();

            let stuck: Vec<StandupItem> = in_progress_items
                .iter()
                .filter(|i| i.updated_at < stuck_cutoff)
                .map(|i| to_standup_item(i))
                .collect();

            let mut by_assignee: std::collections::BTreeMap<String, Vec<StandupItem>> =
                std::collections::BTreeMap::new();
            for i in &in_progress_items {
                by_assignee
                    .entry(
                        i.assignee_agent
                            .clone()
                            .unwrap_or_else(|| "unassigned".into()),
                    )
                    .or_default()
                    .push(to_standup_item(i));
            }
            let in_progress: Vec<StandupGroup> = by_assignee
                .into_iter()
                .map(|(assignee, items)| StandupGroup { assignee, items })
                .collect();

            let resp = StandupResponse {
                cutoff_hours,
                stuck_days,
                done_count: done.len(),
                done,
                in_progress_count: in_progress_items.len(),
                in_progress,
                stuck_count: stuck.len(),
                stuck,
            };
            Ok(serde_json::to_string_pretty(&resp).unwrap_or_default())
        })?
    }

    /// One-call health scorecard: velocity (trailing weekly windows, updated_at
    /// proxy per rubric.md), WIP, stuck, and a bottlenecks placeholder.
    ///
    /// No precomputed/event-populated rollup table backs velocity — checked
    /// first: `events::emit` (agentflare-backend/src/events.rs) is outbound
    /// webhook delivery only, not a persisted log, and there's no handoff-
    /// history table either (`handoff` is assign + asset version + comment,
    /// not a separate audit log). Building either is real new schema/migration
    /// work; at this project's actual scale (~40 items) a live scan is
    /// sub-millisecond (see the groom benchmark), so adding that
    /// infrastructure now would be speculative. Revisit if item volume grows
    /// enough that this scan is ever measured as slow — don't estimate it.
    pub(super) fn item_health(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let window_weeks = req.window_weeks.unwrap_or(4).clamp(1, MAX_WINDOW_WEEKS);
        let stuck_days = req.staleness_days.unwrap_or(7).max(0);
        self.with_backend_db(|conn| {
            let project = self.resolve_project(conn)?;
            let items = agentflare_backend::item::list_by_project(conn, &project.id)
                .map_err(map_backend_err)?;
            let states = agentflare_backend::state::list_by_project(conn, &project.id)
                .map_err(map_backend_err)?;
            let state_by_id: std::collections::HashMap<&str, &agentflare_backend::state::State> =
                states.iter().map(|s| (s.id.as_str(), s)).collect();
            let group_of = |i: &agentflare_backend::item::Item| -> &str {
                state_by_id
                    .get(i.state_id.as_str())
                    .map(|s| s.group_name.as_str())
                    .unwrap_or("")
            };

            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);

            let completed: Vec<&agentflare_backend::item::Item> = items
                .iter()
                .filter(|i| group_of(i) == "completed")
                .collect();
            let mut velocity: Vec<VelocityWeek> = (0..window_weeks)
                .map(|w| {
                    let week_end = now - w.saturating_mul(7 * 86_400);
                    let week_start = week_end - 7 * 86_400;
                    // completed_at, not updated_at (see the standup fix above —
                    // same reason: editing a completed item must not move it
                    // between velocity weeks). Upper bound inclusive: an item
                    // completed in the same second as this call must not be
                    // excluded from "this week".
                    let completed_count = completed
                        .iter()
                        .filter(|i| {
                            i.completed_at
                                .is_some_and(|t| t > week_start && t <= week_end)
                        })
                        .count();
                    VelocityWeek {
                        week_start,
                        week_end,
                        completed_count,
                    }
                })
                .collect();
            velocity.reverse(); // oldest -> newest
            let velocity_trend = match velocity.len() {
                n if n >= 2 => {
                    let last = velocity[n - 1].completed_count;
                    let prev = velocity[n - 2].completed_count;
                    match last.cmp(&prev) {
                        std::cmp::Ordering::Greater => "up",
                        std::cmp::Ordering::Less => "down",
                        std::cmp::Ordering::Equal => "flat",
                    }
                }
                _ => "flat",
            }
            .to_string();

            let wip: Vec<StandupItem> = items
                .iter()
                .filter(|i| group_of(i) == "started")
                .map(to_standup_item)
                .collect();
            let stuck_cutoff = now - stuck_days.saturating_mul(86_400);
            let stuck: Vec<StandupItem> = wip
                .iter()
                .filter(|i| i.updated_at < stuck_cutoff)
                .cloned()
                .collect();

            let resp = HealthResponse {
                window_weeks,
                velocity,
                velocity_trend,
                wip_count: wip.len(),
                wip,
                stuck_days,
                stuck_count: stuck.len(),
                stuck,
                bottlenecks: Vec::new(),
                bottleneck_note: "no handoff history — agentflare does not persist a handoff \
                    log distinct from item state today"
                    .to_string(),
            };
            Ok(serde_json::to_string_pretty(&resp).unwrap_or_default())
        })?
    }
}
