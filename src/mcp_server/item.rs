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

/// Default/max page size for `list` — omitting `limit` used to return every
/// matching item unbounded, which can blow past the MCP response token cap
/// on large projects (155 items / 52k chars observed). Mirrors the
/// `unwrap_or`+`clamp` pattern already used by `search`/`groom` below.
const DEFAULT_LIST_LIMIT: i64 = 50;
const MAX_LIST_LIMIT: i64 = 500;

fn priority_rank(p: &str) -> u8 {
    match p {
        "urgent" => 5,
        "high" => 4,
        "medium" => 3,
        "low" => 2,
        _ => 1,
    }
}

/// Parses the free-form `metadata` JSON blob and unwraps it, defensively,
/// one extra layer if it's double-encoded (observed live — item(create)
/// with metadata={"size":"S"} stored `"{\"size\": \"S\"}"` instead of the
/// object). Shared by every `metadata`-blob field reader below so the
/// double-encoding workaround lives in exactly one place.
///
/// Root cause (now fixed at the write side too, see `metadata_to_json_string`
/// below): some callers send `metadata` as an already-JSON-encoded string
/// rather than a native object; `ItemRequest.metadata: Option<serde_json::Value>`
/// deserializes that as `Value::String(the_json_text)`, and the old
/// `req.metadata.map(|v| v.to_string())` write path blindly re-stringified
/// whatever `Value` variant it got — correct for `Value::Object`, wrong for
/// `Value::String`, which wraps already-valid JSON text in another layer of
/// quotes/escaping. This read-side unwrap stays as a defense for rows
/// written before the write-side fix.
fn parsed_metadata(metadata: &str) -> Option<serde_json::Value> {
    let mut value = serde_json::from_str::<serde_json::Value>(metadata).ok()?;
    if let serde_json::Value::String(inner) = &value
        && let Ok(reparsed) = serde_json::from_str::<serde_json::Value>(inner)
    {
        value = reparsed;
    }
    Some(value)
}

/// Converts a caller-supplied `metadata` value into the JSON string
/// `CreateItem`/`UpdateItem` store. A `Value::String` whose content is
/// itself valid JSON is already-encoded text (see `parsed_metadata`'s doc
/// comment for why that shape shows up) and must be used as-is; anything
/// else -- including a genuine plain-string metadata value like `"hello"`,
/// which is NOT already-encoded JSON -- is serialized normally, same as
/// `Value::to_string()` did. Mirrors `parsed_metadata`'s own "does the
/// inner string reparse as JSON" test so both sides agree on what counts
/// as double-encoded.
fn metadata_to_json_string(value: serde_json::Value) -> String {
    match &value {
        serde_json::Value::String(s) if serde_json::from_str::<serde_json::Value>(s).is_ok() => {
            s.clone()
        }
        _ => value.to_string(),
    }
}

/// `size` lives in the free-form `metadata` JSON blob (`{"size": "S"|"M"|"L"}`)
/// rather than a regex over description prose — sets via `item(update)`.
///
/// `pub(crate)`: also read by `cli::work` to build a `TaskContext` for
/// agent routing.
pub(crate) fn parsed_size(metadata: &str) -> Option<String> {
    parsed_metadata(metadata)?
        .get("size")?
        .as_str()
        .filter(|s| matches!(*s, "S" | "M" | "L"))
        .map(str::to_string)
}

/// `kind` lives in the same `metadata` JSON blob (`{"kind": "locate"}`),
/// same convention as `size` above but free-form rather than a small fixed
/// set — task shape ("locate", "docs", "security-review", ...) isn't
/// enumerable the way S/M/L is. Any non-empty string is accepted.
///
/// `pub(crate)`: read by `cli::work` to build a `TaskContext` for agent
/// routing; nothing sets it yet (no `kind` UI/tool surface), so a
/// `[[router.rule]] when.kind = "..."` rule only ever matches once
/// something writes this metadata key.
pub(crate) fn parsed_kind(metadata: &str) -> Option<String> {
    parsed_metadata(metadata)?
        .get("kind")?
        .as_str()
        .filter(|s| !s.is_empty())
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

    /// [`Self::resolve_item_id`] plus proof that the item actually exists.
    /// `resolve_id` passes a non-numeric id straight through, so an unknown
    /// UUID otherwise reaches the write and surfaces as a raw
    /// "FOREIGN KEY constraint failed" (#375) that names nothing. Use this
    /// wherever an id is about to be written as a foreign key rather than
    /// read back through a call that would 404 on its own.
    pub(crate) fn resolve_existing_item_id(
        &self,
        conn: &Connection,
        raw: &str,
    ) -> Result<String, ErrorData> {
        let id = self.resolve_item_id(conn, raw)?;
        match agentflare_backend::item::get(conn, &id) {
            Ok(_) => Ok(id),
            Err(agentflare_backend::Error::NotFound(_)) => Err(ErrorData::invalid_params(
                format!("no item matches id '{raw}'"),
                None,
            )),
            Err(e) => Err(map_backend_err(e)),
        }
    }

    /// Resolve a target state from exactly one of `state_id`, `state_name`
    /// (case-insensitive exact match), or `state_group`. Errors if none or
    /// more than one of the three is given, or if a name/group doesn't
    /// resolve to exactly one state — see #97: silently accepting a
    /// mis-guessed `state_id` is what caused two wrong live transitions
    /// before the correct one was found by dumping the `states` table.
    fn resolve_state_id(
        &self,
        conn: &Connection,
        project_id: &str,
        state_id: Option<&str>,
        state_name: Option<&str>,
        state_group: Option<&str>,
    ) -> Result<String, ErrorData> {
        let given = [
            state_id.is_some(),
            state_name.is_some(),
            state_group.is_some(),
        ]
        .iter()
        .filter(|x| **x)
        .count();
        if given == 0 {
            return Err(ErrorData::invalid_params(
                "one of state_id, state_name, or state_group is required for update_state",
                None,
            ));
        }
        if given > 1 {
            return Err(ErrorData::invalid_params(
                "state_id, state_name, and state_group are mutually exclusive for update_state",
                None,
            ));
        }
        if let Some(id) = state_id {
            return Ok(id.to_string());
        }
        let states = agentflare_backend::state::list_by_project(conn, project_id)
            .map_err(map_backend_err)?;
        if let Some(name) = state_name {
            let matches: Vec<_> = states
                .iter()
                .filter(|s| s.name.eq_ignore_ascii_case(name))
                .collect();
            return match matches.as_slice() {
                [] => Err(ErrorData::invalid_params(
                    format!("no state named '{name}' in this project"),
                    None,
                )),
                [s] => Ok(s.id.clone()),
                many => Err(ErrorData::invalid_params(
                    format!(
                        "multiple states match name '{name}': {}",
                        many.iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    None,
                )),
            };
        }
        let group = state_group.expect("exactly one of the three is Some");
        let matches: Vec<_> = states.iter().filter(|s| s.group_name == group).collect();
        match matches.as_slice() {
            [] => Err(ErrorData::invalid_params(
                format!("no state in group '{group}' in this project"),
                None,
            )),
            [s] => Ok(s.id.clone()),
            many => Err(ErrorData::invalid_params(
                format!(
                    "multiple states match group '{group}': {}",
                    many.iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                None,
            )),
        }
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
            // A sequence_id or `#`-prefixed id must resolve the same way
            // `update`'s parent_id does (#375/#377) — otherwise it reaches
            // the INSERT's FK column raw and fails as an opaque "FOREIGN
            // KEY constraint failed" instead of naming the bad id.
            let parent_id = match req.parent_id.as_deref() {
                None => None,
                Some(p) if p.trim().is_empty() => None,
                Some(p) => Some(self.resolve_existing_item_id(conn, p)?),
            };
            let input = agentflare_backend::item::CreateItem {
                project_id: project.id,
                state_id,
                name,
                description: req.description,
                priority: req.priority,
                parent_id,
                assignee_agent: req.assignee_agent,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: req.metadata.map(metadata_to_json_string),
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

            let total = items.len();
            let offset = req.offset.unwrap_or(0) as usize;
            let limit = req
                .limit
                .unwrap_or(DEFAULT_LIST_LIMIT)
                .clamp(0, MAX_LIST_LIMIT) as usize;
            let page: Vec<_> = items.into_iter().skip(offset).take(limit).collect();

            let summaries: Vec<ItemSummary> = page
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

            let next_offset = (offset.saturating_add(summaries.len()) < total && limit > 0)
                .then_some(offset.saturating_add(limit));
            let prev_offset = (limit > 0 && offset > 0 && total > 0)
                .then_some(offset.min(total).saturating_sub(limit));

            let page = ItemListPage {
                items: summaries,
                total,
                offset,
                limit,
                next_offset,
                prev_offset,
            };
            Ok(serde_json::to_string_pretty(&page).unwrap_or_default())
        })?
    }

    pub(crate) fn item_update(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for update", None))?;
        if raw.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        self.with_backend_db(|conn| {
            let id = self.resolve_item_id(conn, &raw)?;
            // `parent_id` used to be accepted and silently dropped here (#377).
            // An explicit empty string detaches the item; anything else is a
            // sequence_id or UUID resolved the same way `id` is.
            let parent_id = match req.parent_id.as_deref() {
                None => None,
                Some(p) if p.trim().is_empty() => Some(None),
                Some(p) => Some(Some(self.resolve_existing_item_id(conn, p)?)),
            };
            let input = agentflare_backend::item::UpdateItem {
                name: req.name,
                description: req.description,
                priority: req.priority,
                state_id: None,
                assignee_agent: req.assignee_agent.clone(),
                sort_order: None,
                metadata: req.metadata.map(metadata_to_json_string),
                parent_id,
            };
            let item =
                agentflare_backend::item::update(conn, &id, input).map_err(map_backend_err)?;
            if req.assignee_agent.is_some() {
                crate::claims::reassignment_releases_claim(
                    conn,
                    &id,
                    req.assignee_agent.as_deref(),
                )
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            }
            Ok(serde_json::to_string_pretty(&item).unwrap_or_default())
        })?
    }

    pub(super) fn item_update_state(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for update_state", None))?;
        if raw.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        self.with_backend_db(|conn| {
            let project = self.resolve_project(conn)?;
            fn non_empty(s: &str) -> Option<&str> {
                (!s.trim().is_empty()).then_some(s)
            }
            let state_id = self.resolve_state_id(
                conn,
                &project.id,
                req.state_id.as_deref().and_then(non_empty),
                req.state_name.as_deref().and_then(non_empty),
                req.state_group.as_deref().and_then(non_empty),
            )?;
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
        let (outcome, item_id, item, target_branch, ttl_used) =
            self.with_backend_db(|conn| {
                let item_id = self.resolve_item_id(conn, &raw)?;
                // Read again (cheap) so a `Held` response can report the TTL it
                // was actually gated by -- `item::claim` computes this internally
                // for in-review items but doesn't hand it back (item #108).
                let ttl_used = agentflare_backend::claim::effective_ttl_secs(conn, &item_id, ttl);
                let outcome = agentflare_backend::item::claim(conn, &item_id, &owner, now, ttl)
                    .map_err(map_backend_err)?;
                let (item, target_branch) =
                    if outcome == agentflare_backend::item::ClaimOutcome::Acquired {
                        let item = agentflare_backend::item::get(conn, &item_id).ok();
                        let target_branch = item
                            .as_ref()
                            .map(|i| crate::worktree::resolve_target_branch(conn, i, &repo_root));
                        (item, target_branch)
                    } else {
                        (None, None)
                    };
                Ok::<_, ErrorData>((outcome, item_id, item, target_branch, ttl_used))
            })??;
        let worktree_result = match (&item, &target_branch) {
            (Some(item), Some(target)) => Some(
                PROGRESS_SENDER
                    .try_with(|ps| {
                        crate::worktree::create_worktree(item, &repo_root, target, ps.as_ref())
                    })
                    .unwrap_or_else(|_| {
                        crate::worktree::create_worktree(item, &repo_root, target, None)
                    }),
            ),
            _ => None,
        };
        Ok(match outcome {
            agentflare_backend::item::ClaimOutcome::Acquired => {
                let mut resp = serde_json::json!({
                    "status": "acquired",
                    "item_id": item_id,
                    "owner": owner,
                });
                // Surface *why* no worktree_path came back instead of silently
                // omitting the field — a swallowed error here reads to the
                // caller as an unexplained circular claim/worktree block
                // (confirmed live: a stale daemon's `git_binary()` PATH-shim
                // self-deny, see PR #304, produced exactly this symptom).
                match worktree_result {
                    Some(Ok(path)) => {
                        resp["worktree_path"] =
                            serde_json::Value::String(path.to_string_lossy().to_string());
                    }
                    Some(Err(e)) => {
                        resp["worktree_error"] = serde_json::Value::String(e);
                    }
                    None => {}
                }
                resp.to_string()
            }
            agentflare_backend::item::ClaimOutcome::Held {
                owner: holder,
                age_secs,
            } => serde_json::json!({"status": "held", "item_id": item_id, "owner": holder, "age_secs": age_secs, "ttl_secs": ttl_used}).to_string(),
            agentflare_backend::item::ClaimOutcome::BlockedByAssignee { assignee } => {
                serde_json::json!({
                    "status": "blocked",
                    "item_id": item_id,
                    "assignee": assignee,
                    "reason": format!("this item was handed off to '{assignee}' and hasn't been accepted yet — only {assignee} can claim it until they accept, decline, or the handoff is cancelled"),
                })
                .to_string()
            }
        })
    }

    pub(crate) fn item_heartbeat(&self, req: ItemRequest) -> Result<String, ErrorData> {
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
        let now = crate::claims::now();
        let ttl = backend_claim_ttl_secs();
        let repo_root = self.worktree_repo_root();
        // Confirm we still hold the claim, and clean up the worktree,
        // *before* actually releasing -- releasing first (as this used to)
        // opens a window where a concurrent `claim` grabs the item and
        // starts working in the worktree just before `cleanup_worktree`
        // deletes it out from under them. Same ordering `done`'s no-PR
        // path uses (cleanup, then release the claim last).
        let (item_id, item, owns_claim) = self.with_backend_db(|conn| {
            let item_id = self.resolve_item_id(conn, &raw)?;
            let item = agentflare_backend::item::get(conn, &item_id).ok();
            let owns_claim = agentflare_backend::claim::is_owner(conn, &item_id, &owner)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            if !owns_claim {
                // Not ours -- steal the lease only if it's abandoned, using
                // the exact same stale-TTL gate `claim()` already uses to
                // steal on acquire. A live claim held by someone else is a
                // real identity conflict and must say so -- item #83:
                // `release` used to return `{"released": false}` either
                // way, making "nothing to release" indistinguishable from
                // "someone else is actively using this" and leaving
                // abandoned claims permanently un-releasable by anyone but
                // a process that no longer exists.
                let ttl = agentflare_backend::claim::effective_ttl_secs(conn, &item_id, ttl);
                if let agentflare_backend::claim::Acquire::Held {
                    owner: holder,
                    age_secs,
                } = agentflare_backend::claim::acquire(conn, &item_id, &owner, now, ttl)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                {
                    return Err(ErrorData::invalid_params(
                        format!(
                            "item {item_id} is claimed by '{holder}' (active {age_secs}s ago, ttl {ttl}s) -- refusing to release someone else's live claim"
                        ),
                        None,
                    ));
                }
            }
            Ok::<_, ErrorData>((item_id, item, owns_claim))
        })??;
        // A plain `release` used to leave the worktree behind unconditionally
        // -- `done`/`check_merge` were the only paths that ever cleaned one
        // up, so an item released without going through either (abandoned,
        // or completed by hand) orphaned its `.worktrees/task/<id>` forever
        // (item #335). `cleanup_worktree` itself still refuses a dirty tree,
        // so this is safe to attempt unconditionally here, before the claim
        // is actually given up.
        if owns_claim && let Some(item) = &item {
            crate::worktree::cleanup_worktree(item, &repo_root);
        }
        let ok = self.with_backend_db(|conn| {
            agentflare_backend::item::release(conn, &item_id, &owner).map_err(map_backend_err)
        })??;
        Ok(serde_json::json!({"released": ok, "item_id": item_id}).to_string())
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
        let ttl = backend_claim_ttl_secs();
        let repo_root = self.worktree_repo_root();
        // Resolve + authorize (DB reads) under the backend lock, then run
        // the blocking git/gh push+PR outside it — `git push`/`gh pr
        // create` have no business running while the shared DB mutex is
        // held. Unlike before, the state transition itself (completed vs
        // in_review) is decided AFTER the push below, since it depends on
        // whether a PR actually results — deciding it up front showed the
        // item as "Completed" for the entire push/PR network round trip
        // even when a PR ends up open and unreviewed (item #420).
        let (item_id, owns_claim, item, target_branch) = self.with_backend_db(|conn| {
            let item_id = self.resolve_item_id(conn, &raw)?;
            let mut owns_claim = agentflare_backend::claim::is_owner(conn, &item_id, &owner)
                .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
            if !owns_claim {
                // Same abandoned-claim steal `item_release` uses (item
                // #83): a live claim held by someone else is a real
                // identity conflict (error, don't silently no-op); a
                // stale/absent one is fair game, and stealing it here
                // means the rest of `done` below runs exactly as if we'd
                // claimed it ourselves.
                owns_claim = match agentflare_backend::claim::acquire(conn, &item_id, &owner, now, ttl)
                    .map_err(|e| ErrorData::internal_error(e.to_string(), None))?
                {
                    agentflare_backend::claim::Acquire::Held {
                        owner: holder,
                        age_secs,
                    } => {
                        return Err(ErrorData::invalid_params(
                            format!(
                                "item {item_id} is claimed by '{holder}' (active {age_secs}s ago) -- refusing to complete someone else's live claim"
                            ),
                            None,
                        ));
                    }
                    agentflare_backend::claim::Acquire::Acquired => true,
                };
            }
            let (item, target_branch) = if owns_claim {
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
            Ok::<_, ErrorData>((item_id, owns_claim, item, target_branch))
        })??;
        let should_push = req.push.unwrap_or(true);
        let summary = req.summary.as_deref();
        // An agent can make real file edits and still exit without ever
        // running `git commit` itself -- with no commit, the branch never
        // diverges from target, so `nothing_was_ever_committed` below can't
        // tell that apart from a genuine no-op, and the edits are silently
        // stranded in the worktree while `done` still reports success
        // (item #57). Commit them here, before push/PR or the "nothing was
        // committed" classification ever run, using the agent's own summary
        // as the commit message when there is one.
        if let Some(item) = &item {
            let message = summary
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("Auto-committed by item done: uncommitted changes at completion");
            match crate::worktree::commit_uncommitted(item, &repo_root, message) {
                crate::worktree::CommitOutcome::Committed => {
                    eprintln!("worktree: auto-committed uncommitted changes for item {item_id}");
                }
                crate::worktree::CommitOutcome::NothingToCommit => {}
                // A dirty tree existed but `add`/`commit` itself failed
                // (item #88's read-only `.git` under bwrap was one cause;
                // disk-full, a rejecting pre-commit hook, or a git config
                // issue could all reproduce it) -- this must NOT fall
                // through to `nothing_was_ever_committed` below, which
                // reads "no new commit" as "genuinely nothing to do" and
                // releases the claim + cleans up the worktree as if the
                // tree were clean. The real edits are still sitting
                // uncommitted in the worktree; surface that loudly instead
                // (item #92).
                crate::worktree::CommitOutcome::Failed(err) => {
                    let comment_body = format!(
                        "## agentflare work — commit failed\n\nAuto-commit of uncommitted \
                         changes failed:\n\n```\n{err}\n```\n\nThe work is still sitting \
                         uncommitted in the item's worktree; it was left in place rather than \
                         reported as done."
                    );
                    let _ = self.comment_impl(CommentRequest {
                        action: "create".into(),
                        item_id: Some(item_id.clone()),
                        body: Some(comment_body),
                        ..Default::default()
                    });
                    return Err(ErrorData::internal_error(
                        format!("item {item_id}: auto-commit of uncommitted changes failed: {err}"),
                        None,
                    ));
                }
            }
        }
        let agent = crate::claims::agent_of(&owner);
        let pr_url = match (&item, &target_branch) {
            (Some(item), Some(target)) if should_push => PROGRESS_SENDER
                .try_with(|ps| {
                    crate::worktree::push_and_open_pr(
                        item,
                        agent,
                        &repo_root,
                        target,
                        ps.as_ref(),
                        summary,
                    )
                })
                .unwrap_or_else(|_| {
                    crate::worktree::push_and_open_pr(
                        item,
                        agent,
                        &repo_root,
                        target,
                        None,
                        summary,
                    )
                }),
            _ => None,
        };
        // An open PR (freshly created, or already existed) means the work
        // isn't actually finished: move to "in_review" instead of
        // "completed", leave the claim lease held (so nobody else can
        // claim it out from under the pending review), and leave the
        // worktree in place — follow-up commits (review comments, CI
        // fixes) may still need to land on this exact branch, which is
        // impossible once the worktree is gone. We nearly hit this
        // ourselves fixing #381/#382's own CodeRabbit findings. Promotion
        // to "completed" (and the worktree cleanup that comes with it)
        // happens once `check_merge` confirms the PR actually merged.
        //
        // No PR resulting means there's nothing pending: go straight to
        // "completed", release the lease, and it's safe to clean up now.
        // `cleanup_worktree` itself still checks for a clean tree and
        // no-ops (logging) rather than trusting push/PR success alone as
        // proof nothing would be lost.
        let in_review = owns_claim && pr_url.is_some();
        let diverged = item
            .as_ref()
            .zip(target_branch.as_ref())
            .is_some_and(|(i, t)| crate::worktree::branch_diverged(i, &repo_root, t));
        // No PR resulted — either nothing was ever committed on the claimed
        // branch, or a real commit's push/PR failed for some other reason.
        // Only the former should block completion: marking an item
        // "completed" when its branch never diverged from target claims
        // work was delivered when none was (item #48) — a headless run that
        // merely replies with text, with no tool use, previously exited 0
        // and sailed straight through to `mark_completed` below with zero
        // code changed.
        let nothing_was_ever_committed = !in_review && owns_claim && !diverged;
        // A real commit exists but push/PR creation soft-failed and never
        // produced a PR (item #109) -- hard-error instead of completing.
        let push_or_pr_failed = !in_review && owns_claim && should_push && diverged;
        if push_or_pr_failed {
            let _ = self.comment_impl(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.clone()),
                body: Some(format!(
                    "## agentflare work — PR creation failed\n\nThe branch has real commits but no pull request resulted; check server logs for item {item_id}. Left in place rather than completed."
                )),
                ..Default::default()
            });
            return Err(ErrorData::internal_error(
                format!("item {item_id}: real commits but no PR resulted -- not marking completed"),
                None,
            ));
        }
        // Shared by both "nothing was ever committed" (worktree is clean by
        // definition, safe to remove) and a real completion: release the
        // lease so the item is genuinely available again — either for a
        // fresh claim (nothing done) or because the work is done — instead
        // of leaving it wedged on a claim nobody will ever release.
        // `cleanup_worktree` itself still verifies the tree is clean before
        // removing it, so this is safe even if `nothing_was_ever_committed`
        // somehow raced with an uncommitted local change.
        let release_claim_and_cleanup = |item: &Option<agentflare_backend::item::Item>| {
            if let Some(item) = item {
                crate::worktree::cleanup_worktree(item, &repo_root);
            }
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
        };
        let done = if !owns_claim {
            false
        } else if nothing_was_ever_committed {
            release_claim_and_cleanup(&item);
            false
        } else if in_review {
            let marked = self.with_backend_db(|conn| {
                agentflare_backend::item::mark_in_review(conn, &item_id, &owner)
                    .map_err(map_backend_err)
            })??;
            // `mark_in_review` silently no-ops (`Ok(false)`) when its own
            // ownership check fails -- normally because the claim lease
            // lapsed (or was reclaimed) between the top of this function and
            // here. `status` below used to be derived from `in_review`
            // regardless, so this looked like a clean success even though a
            // PR now exists (`pr_url` above) while the item's own state
            // never moved to in_review and the lease was never actually
            // held on our behalf -- silently stranding it for a stale-sweep
            // to pick back up. Loud is correct here: the push/PR already
            // happened, so this can't be quietly retried.
            if !marked {
                return Err(ErrorData::internal_error(
                    format!(
                        "item {item_id}: PR was opened but the claim was lost before the item \
                         could be marked in_review (owner mismatch) -- item state was NOT \
                         updated"
                    ),
                    None,
                ));
            }
            marked
        } else {
            let moved = self.with_backend_db(|conn| {
                agentflare_backend::item::mark_completed(conn, &item_id, &owner)
                    .map_err(map_backend_err)
            })??;
            if moved {
                release_claim_and_cleanup(&item);
            }
            moved
        };
        let status = if in_review {
            "in_review"
        } else if done {
            "completed"
        } else {
            "unchanged"
        };
        let mut resp = serde_json::json!({"done": done, "item_id": item_id, "status": status});
        if let Some(url) = pr_url {
            resp["pr_url"] = serde_json::Value::String(url.clone());
        }
        Ok(resp.to_string())
    }

    /// Promotes an item sitting in "in_review" to "completed" once its PR
    /// is confirmed merged — the other half of `done`'s split (item #420).
    /// Idempotent and cheap to call speculatively: a no-op (not an error)
    /// when the item isn't currently in_review, and soft-fails (never
    /// errors) when GitHub can't be reached, same as `push_and_open_pr`.
    /// Also removes the item's worktree once promoted, same safety check
    /// (`cleanup_worktree`) as `done`'s no-PR path uses.
    ///
    /// `pub(crate)`: also called directly by `supervisor::run_review_sweep`
    /// (item #65) once it sees `worktree::PrCiStatus::Merged`, the same way
    /// `item_claim` is already called directly from `cli::work`.
    pub(crate) fn item_check_merge(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let raw = req
            .id
            .ok_or_else(|| ErrorData::invalid_params("id is required for check_merge", None))?;
        if raw.trim().is_empty() {
            return Err(ErrorData::invalid_params("id is required", None));
        }
        let repo_root = self.worktree_repo_root();
        let (item_id, item, in_review) = self.with_backend_db(|conn| {
            let item_id = self.resolve_item_id(conn, &raw)?;
            let item = agentflare_backend::item::get(conn, &item_id).map_err(map_backend_err)?;
            let state =
                agentflare_backend::state::get(conn, &item.state_id).map_err(map_backend_err)?;
            let in_review = state.group_name == "in_review";
            Ok::<_, ErrorData>((item_id, item, in_review))
        })??;
        if !in_review {
            return Ok(serde_json::json!({
                "item_id": item_id,
                "promoted": false,
                "reason": "item is not in_review",
            })
            .to_string());
        }
        if !crate::worktree::is_pr_merged(&item, &repo_root) {
            return Ok(serde_json::json!({
                "item_id": item_id,
                "promoted": false,
                "reason": "PR not merged yet",
            })
            .to_string());
        }
        let promoted = self.with_backend_db(|conn| {
            agentflare_backend::item::promote_in_review_to_completed(conn, &item_id)
                .map_err(map_backend_err)
        })??;
        if promoted {
            crate::worktree::cleanup_worktree(&item, &repo_root);
            crate::worktree::relabel_pr_completed(&item, &repo_root);
        }
        Ok(serde_json::json!({"item_id": item_id, "promoted": promoted}).to_string())
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

    pub(crate) fn item_add_label(&self, req: ItemRequest) -> Result<String, ErrorData> {
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

    pub(crate) fn item_remove_label(&self, req: ItemRequest) -> Result<String, ErrorData> {
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
            let project = self.resolve_project_for_read(conn, req.project.as_deref())?;
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
            let project = self.resolve_project_for_read(conn, req.project.as_deref())?;
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
    /// proxy per rubric.md), WIP, stuck, and bottlenecks (items handed between
    /// agents ≥2× in the window, from the `item_assignment_events` log written
    /// by `item::update` — history starts at that migration, so transitions
    /// predating it are not counted).
    ///
    /// No precomputed rollup table backs velocity — at this project's actual
    /// scale (~40 items) a live scan is sub-millisecond (see the groom
    /// benchmark). Revisit if item volume grows enough that this scan is ever
    /// measured as slow — don't estimate it.
    pub(super) fn item_health(&self, req: ItemRequest) -> Result<String, ErrorData> {
        let window_weeks = req.window_weeks.unwrap_or(4).clamp(1, MAX_WINDOW_WEEKS);
        let stuck_days = req.staleness_days.unwrap_or(7).max(0);
        self.with_backend_db(|conn| {
            let project = self.resolve_project_for_read(conn, req.project.as_deref())?;
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

            let window_start = now - window_weeks.saturating_mul(7 * 86_400);
            let handoff_stats = agentflare_backend::assignment_events::handoff_stats_since(
                conn,
                &project.id,
                window_start,
            )
            .map_err(map_backend_err)?;
            let item_by_id: std::collections::HashMap<&str, &agentflare_backend::item::Item> =
                items.iter().map(|i| (i.id.as_str(), i)).collect();
            let bottlenecks: Vec<String> = handoff_stats
                .iter()
                .filter(|s| s.handoffs >= 2)
                .map(|s| {
                    let label = item_by_id
                        .get(s.item_id.as_str())
                        .map(|i| format!("#{} {}", i.sequence_id, i.name))
                        .unwrap_or_else(|| s.item_id.clone());
                    format!(
                        "{label} — {} handoffs ({})",
                        s.handoffs,
                        s.owners.join(" → ")
                    )
                })
                .collect();
            let bottleneck_note = if bottlenecks.is_empty() {
                "no item was handed between agents ≥2× in the window (handoff history \
                 is recorded from the assignment-log migration onward — earlier \
                 transitions are not counted)"
                    .to_string()
            } else {
                format!(
                    "items handed between agents ≥2× in the last {window_weeks} week(s) — \
                     repeated handoffs usually mean unclear ownership or a stuck dependency"
                )
            };

            let resp = HealthResponse {
                window_weeks,
                velocity,
                velocity_trend,
                wip_count: wip.len(),
                wip,
                stuck_days,
                stuck_count: stuck.len(),
                stuck,
                bottlenecks,
                bottleneck_note,
            };
            Ok(serde_json::to_string_pretty(&resp).unwrap_or_default())
        })?
    }
}

#[cfg(test)]
mod metadata_field_tests {
    use super::*;

    fn double_encoded(value: serde_json::Value) -> String {
        serde_json::Value::String(value.to_string()).to_string()
    }

    #[test]
    fn parsed_kind_reads_a_plain_object() {
        assert_eq!(
            parsed_kind(r#"{"kind":"locate"}"#),
            Some("locate".to_string())
        );
    }

    #[test]
    fn parsed_kind_survives_double_encoded_metadata() {
        let metadata = double_encoded(serde_json::json!({"kind": "docs"}));
        assert_eq!(parsed_kind(&metadata), Some("docs".to_string()));
    }

    #[test]
    fn parsed_kind_rejects_empty_string() {
        assert_eq!(parsed_kind(r#"{"kind":""}"#), None);
    }

    #[test]
    fn parsed_kind_none_when_absent_or_metadata_is_not_json() {
        assert_eq!(parsed_kind("{}"), None);
        assert_eq!(parsed_kind("not json"), None);
    }

    #[test]
    fn parsed_size_still_rejects_values_outside_s_m_l() {
        assert_eq!(parsed_size(r#"{"size":"XL"}"#), None);
        assert_eq!(parsed_size(r#"{"size":"M"}"#), Some("M".to_string()));
    }

    #[test]
    fn parsed_size_survives_double_encoded_metadata() {
        let metadata = double_encoded(serde_json::json!({"size": "S"}));
        assert_eq!(parsed_size(&metadata), Some("S".to_string()));
    }

    #[test]
    fn metadata_to_json_string_serializes_a_plain_object_once() {
        let value = serde_json::json!({"goal": {"objective": "ship it"}});
        let stored = metadata_to_json_string(value.clone());
        assert_eq!(stored, value.to_string());
        // Single-encoded: parses straight to an object, no extra unwrap needed.
        let reparsed: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert!(reparsed.is_object());
    }

    #[test]
    fn metadata_to_json_string_does_not_double_encode_a_pre_stringified_value() {
        // What a caller sending metadata as an already-JSON-encoded string
        // (rather than a native object) produces once `ItemRequest.metadata:
        // Option<serde_json::Value>` deserializes it: Value::String(json_text).
        let pre_stringified =
            serde_json::Value::String(r#"{"goal":{"objective":"ship it"}}"#.to_string());
        let stored = metadata_to_json_string(pre_stringified);
        assert_eq!(stored, r#"{"goal":{"objective":"ship it"}}"#);
        let reparsed: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert!(
            reparsed.is_object(),
            "must parse straight to an object, not a string wrapping one: {reparsed:?}"
        );
    }

    #[test]
    fn metadata_to_json_string_still_json_encodes_a_genuine_plain_string() {
        // metadata: "hello" is a real (if unusual) case -- "hello" is not
        // itself valid JSON, so it must NOT be used verbatim; it needs to
        // stay wrapped as the JSON string "hello" like a plain
        // Value::to_string() would produce, or the stored metadata column
        // stops being valid JSON at all.
        let stored = metadata_to_json_string(serde_json::Value::String("hello".to_string()));
        assert_eq!(stored, "\"hello\"");
        let reparsed: serde_json::Value = serde_json::from_str(&stored).unwrap();
        assert_eq!(reparsed, serde_json::Value::String("hello".to_string()));
    }
}
