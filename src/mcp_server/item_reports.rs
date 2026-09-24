//! `item` groom/standup/health reporting actions -- split out of `item.rs`
//! to keep it under the repo's LOC gate. Moved verbatim; the per-item
//! annotation pass they share with `list`/`search` (`compute_annotations`)
//! stays in `item.rs`.

use super::item::compute_annotations;
use super::*;

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
        if i.annotations.unestimated {
            needs_estimation.push(i.id.clone());
        } else if !i.annotations.blocked_by.is_empty() {
            later.push(i.id.clone());
        } else {
            ready.push(i.id.clone());
        }
    }
    let next = ready.split_off(capacity.min(ready.len()));
    (ready, next, later, needs_estimation)
}

impl AgentflareMcp {
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

            let claim_ttl_secs = backend_claim_ttl_secs();
            let mut annotations_by_id = compute_annotations(
                conn,
                &shortlist,
                &state_by_id,
                now,
                stale_cutoff,
                claim_ttl_secs,
            )?;

            let groom_items: Vec<GroomItem> = shortlist
                .into_iter()
                .map(|i| {
                    let state = state_by_id.get(i.state_id.as_str());
                    let annotations = annotations_by_id
                        .remove(&i.id)
                        .expect("compute_annotations returns an entry for every input item");
                    GroomItem {
                        id: i.id,
                        sequence_id: i.sequence_id,
                        name: i.name,
                        description: i.description,
                        state: state.map(|s| s.name.clone()).unwrap_or_default(),
                        state_group: state.map(|s| s.group_name.clone()).unwrap_or_default(),
                        priority: i.priority,
                        assignee_agent: i.assignee_agent,
                        updated_at: i.updated_at,
                        due_date: i.due_date,
                        annotations,
                    }
                })
                .collect();

            let pull_next: Vec<String> = groom_items
                .iter()
                .filter(|i| {
                    i.annotations.unassigned
                        && !i.annotations.stale
                        && i.annotations.blocked_by.is_empty()
                })
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
                stale_count: groom_items.iter().filter(|i| i.annotations.stale).count(),
                unassigned_count: groom_items
                    .iter()
                    .filter(|i| i.annotations.unassigned)
                    .count(),
                unestimated_count: groom_items
                    .iter()
                    .filter(|i| i.annotations.unestimated)
                    .count(),
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
