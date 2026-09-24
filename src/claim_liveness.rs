//! Claim-owner liveness: releases an item claim within a couple of
//! supervisor ticks of its owner dying, instead of leaving it held for the
//! multi-hour item-claim TTL (`backend_claim_ttl_secs`). A killed `agentflare
//! work`, a crashed interactive session or a job cancelled out from under
//! its lease otherwise keeps the item non-dispatchable for up to 4h.
//!
//! An owner is judged, first match wins:
//! 1. a daemon job owner `<agent>:<job-id>` by its `agent_jobs` row -- a
//!    finished job is dead; a queued one is live; a running one is live
//!    unless its registered session says its process is gone;
//! 2. the live session registry (`crate::sessions`);
//! 3. the per-process fallback owner `<agent>:<pid>-<hex>`
//!    (`claims::owner_id`) by whether that pid still runs here -- only
//!    after the claim has also been silent for `sessions::STALE_AFTER_SECS`,
//!    because the claim ledger doesn't record which host a pid lives on;
//! 4. otherwise unknown, which keeps today's TTL behavior.
//!
//! A dead verdict must repeat on two consecutive sweeps before anything is
//! released, so a session that is just registering can't lose its claim to
//! a single racy read.

use rusqlite::Connection;

/// What the sweep concluded about a claim owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OwnerLiveness {
    Live,
    /// Dead, with a short human-readable reason.
    Dead(String),
    Unknown,
}

/// The instance half of an `<agent>:<instance>` owner.
fn instance_of(owner: &str) -> Option<&str> {
    owner.split_once(':').map(|(_, instance)| instance)
}

/// The pid of a `claims::process_instance_id`-style instance
/// (`<pid>-<16 hex>`), or `None` for any other instance shape.
fn fallback_pid(instance: &str) -> Option<u32> {
    let (pid, suffix) = instance.split_once('-')?;
    if suffix.len() != 16 || !suffix.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    pid.parse().ok()
}

fn job_state_name(state: agentflare_jobs::JobState) -> &'static str {
    match state {
        agentflare_jobs::JobState::Queued => "queued",
        agentflare_jobs::JobState::Running => "running",
        agentflare_jobs::JobState::Exited => "finished",
        agentflare_jobs::JobState::Killed => "cancelled",
        agentflare_jobs::JobState::Failed => "failed",
    }
}

/// Judges one claim owner. `heartbeat_at` is the claim's last heartbeat;
/// `sessions` is `agentflare.db` (the session registry), `queue` the job
/// queue -- either may be absent, which only makes the verdict less certain.
pub(crate) fn judge_owner(
    owner: &str,
    heartbeat_at: i64,
    now: i64,
    sessions: Option<&Connection>,
    queue: Option<&agentflare_jobs::Queue>,
) -> OwnerLiveness {
    let instance = instance_of(owner);
    let session = || {
        sessions
            .and_then(|conn| crate::sessions::liveness(conn, owner, now).ok())
            .unwrap_or(crate::sessions::Liveness::Unknown)
    };
    if let Some(id) = instance {
        // A job executing in this very process is live by definition, even
        // if its session row hasn't been written yet.
        if agentflare_jobs::cancel::is_registered(id) {
            return OwnerLiveness::Live;
        }
        if let Some(job) = queue.and_then(|q| q.get(id).ok()) {
            return match job.state {
                agentflare_jobs::JobState::Queued => OwnerLiveness::Live,
                agentflare_jobs::JobState::Running => match session() {
                    crate::sessions::Liveness::Dead => OwnerLiveness::Dead(format!(
                        "job {id} is marked running but its process has exited"
                    )),
                    _ => OwnerLiveness::Live,
                },
                state => OwnerLiveness::Dead(format!("job {id} is {}", job_state_name(state))),
            };
        }
    }
    match session() {
        crate::sessions::Liveness::Live => return OwnerLiveness::Live,
        crate::sessions::Liveness::Dead => {
            return OwnerLiveness::Dead("its session has ended".to_string());
        }
        crate::sessions::Liveness::Unknown => {}
    }
    if let Some(pid) = instance.and_then(fallback_pid) {
        if crate::ipc::process::is_alive(pid) {
            return OwnerLiveness::Live;
        }
        if now - heartbeat_at > crate::sessions::STALE_AFTER_SECS {
            return OwnerLiveness::Dead(format!("process {pid} is no longer running"));
        }
    }
    OwnerLiveness::Unknown
}

/// Dead verdicts seen on the previous sweep, keyed by `(item_id, owner)`.
/// Only the previous sweep's sightings are kept, so a verdict that flips
/// back to live (or unknown) in between starts the confirmation over.
#[derive(Debug, Default)]
pub(crate) struct SweepMemory {
    suspects: std::collections::HashSet<(String, String)>,
}

/// One claim the sweep released.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Released {
    pub item_id: String,
    pub owner: String,
    pub reason: String,
    /// Whether the item was re-armed (`ready-for-work`) for redispatch.
    pub redispatched: bool,
}

/// One liveness sweep over every active item claim in `backend`. Releases a
/// claim only when its owner was judged dead on this sweep AND the previous
/// one, then restores the item for redispatch (see `restore_item`).
pub(crate) fn sweep(
    backend: &Connection,
    sessions: Option<&Connection>,
    queue: Option<&agentflare_jobs::Queue>,
    memory: &mut SweepMemory,
    now: i64,
) -> Vec<Released> {
    let ttl = crate::mcp_server::types::backend_claim_ttl_secs();
    let claims = match agentflare_backend::claim::list_all(backend, now, ttl) {
        Ok(claims) => claims,
        Err(e) => {
            eprintln!("agentflare-supervisor: claim liveness sweep could not list claims: {e}");
            return Vec::new();
        }
    };
    let mut suspects = std::collections::HashSet::new();
    let mut released = Vec::new();
    for claim in claims {
        if claim.status != "claimed" || claim.key.len() != 1 {
            continue;
        }
        let item_id = claim.key[0].clone();
        let OwnerLiveness::Dead(reason) =
            judge_owner(&claim.owner, claim.heartbeat_at, now, sessions, queue)
        else {
            continue;
        };
        let key = (item_id.clone(), claim.owner.clone());
        if !memory.suspects.contains(&key) {
            suspects.insert(key);
            continue;
        }
        // Owner-scoped: if someone else claimed the item in the meantime
        // this releases nothing.
        match agentflare_backend::claim::release(backend, &item_id, &claim.owner) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => {
                eprintln!(
                    "agentflare-supervisor: could not release dead claim on {item_id} held by {}: {e}",
                    claim.owner
                );
                suspects.insert(key);
                continue;
            }
        }
        let redispatched = restore_item(backend, &item_id, &claim.owner, &reason);
        released.push(Released {
            item_id,
            owner: claim.owner,
            reason,
            redispatched,
        });
    }
    memory.suspects = suspects;
    released
}

/// After a dead owner's claim is released: comment once on the item and,
/// when it is still unfinished work, re-arm it for the discovery tick via
/// `item::redispatch` (backlog + `ready-for-work`, stale dispatch labels
/// cleared). Items that must not be re-queued are only commented on:
/// finished or cancelled items, items in review (their PR is the work), and
/// items an operator parked (`paused`, `needs-manual-dispatch`). Once the
/// any-reason dispatch-failure ceiling is reached the item goes to
/// `needs-manual-dispatch` instead, same as the orphan reconcile does.
/// Returns whether the item was re-armed.
fn restore_item(conn: &Connection, item_id: &str, owner: &str, reason: &str) -> bool {
    let Ok(item) = agentflare_backend::item::get(conn, item_id) else {
        return false;
    };
    let group = agentflare_backend::state::get(conn, &item.state_id)
        .map(|s| s.group_name)
        .unwrap_or_default();
    let labels =
        agentflare_backend::label::list_by_project(conn, &item.project_id).unwrap_or_default();
    let label_id = |name: &str| labels.iter().find(|l| l.name == name).map(|l| l.id.clone());
    let item_labels = agentflare_backend::item::list_labels(conn, item_id).unwrap_or_default();
    let has_label = |name: &str| label_id(name).is_some_and(|id| item_labels.contains(&id));
    let parked = has_label(crate::supervisor::PAUSED_LABEL)
        || has_label(crate::supervisor::NEEDS_MANUAL_LABEL);
    let finished = matches!(group.as_str(), "completed" | "cancelled" | "in_review");
    let at_cap = agentflare_backend::comment::list_by_item(conn, item_id)
        .map(|comments| {
            crate::dispatch_failure_ceiling::consecutive_failure_count_any_reason(&comments)
                >= crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP_ANY_REASON
        })
        .unwrap_or(false);

    let mut redispatched = false;
    let mut outcome = String::new();
    if finished || parked || item.assignee_agent.is_none() {
        // Nothing to re-queue; the release alone frees the item.
    } else if at_cap {
        for name in [
            crate::supervisor::READY_LABEL,
            crate::supervisor::DISPATCHED_LABEL,
        ] {
            if let Some(id) = label_id(name) {
                let _ = agentflare_backend::item::remove_label(conn, item_id, &id);
            }
        }
        if let Some(id) = label_id(crate::supervisor::NEEDS_MANUAL_LABEL) {
            let _ = agentflare_backend::item::add_label(conn, item_id, &id);
        }
        outcome = " The dispatch-failure ceiling is reached, so it is parked on \
                   `needs-manual-dispatch` instead of being re-queued."
            .to_string();
    } else {
        redispatched = matches!(
            agentflare_backend::item::redispatch(conn, item_id, None),
            Ok(agentflare_backend::item::RedispatchOutcome::Ready { .. })
        );
        if redispatched {
            outcome = " Re-queued for dispatch.".to_string();
        }
    }
    let body = format!(
        "## supervisor — claim released\n\nclaim released: owner {owner} is no longer running \
         ({reason}).{outcome}"
    );
    let _ = agentflare_backend::comment::create(conn, item_id, "agentflare-supervisor", &body);
    redispatched
}

/// The daemon's per-tick entry point (see `dashboard::server`'s discovery
/// ticker): one sweep across every project's claims, with the dead-verdict
/// memory carried between ticks. Also prunes long-dead session rows.
pub(crate) fn run_sweep(
    mcp: &crate::mcp_server::AgentflareMcp,
    queue: &agentflare_jobs::Queue,
) -> Vec<Released> {
    static MEMORY: std::sync::LazyLock<std::sync::Mutex<SweepMemory>> =
        std::sync::LazyLock::new(Default::default);
    let now = crate::claims::now();
    let sessions = crate::db::open().ok();
    if let Some(conn) = &sessions {
        let _ = crate::sessions::prune(conn, now);
    }
    let mut memory = MEMORY.lock().unwrap_or_else(|e| e.into_inner());
    let released = mcp
        .with_backend_db(|conn| sweep(conn, sessions.as_ref(), Some(queue), &mut memory, now))
        .unwrap_or_default();
    for r in &released {
        eprintln!(
            "agentflare-supervisor: released claim on item {} held by {} ({}){}",
            r.item_id,
            r.owner,
            r.reason,
            if r.redispatched {
                "; re-queued for dispatch"
            } else {
                ""
            }
        );
    }
    released
}

/// Registers a headless work-item run in the session registry under its
/// claim owner, with this process's pid, so claim liveness can judge the
/// run exactly (pid check) instead of by heartbeat age. Ends the session on
/// drop unless `keep_on_drop` was called. Best-effort throughout: a registry
/// write failure must never fail the run.
pub(crate) struct HeadlessSession {
    key: String,
    item_id: String,
    cwd: String,
    end_on_drop: bool,
}

impl HeadlessSession {
    pub(crate) fn register(key: &str, item_id: &str, cwd: &str) -> Self {
        let session = Self {
            key: key.to_string(),
            item_id: item_id.to_string(),
            cwd: cwd.to_string(),
            end_on_drop: true,
        };
        session.touch();
        session
    }

    pub(crate) fn touch(&self) {
        if let Ok(conn) = crate::db::open() {
            let _ = crate::sessions::touch(
                &conn,
                &crate::sessions::Touch {
                    key: &self.key,
                    name: None,
                    item_id: Some(&self.item_id),
                    cwd: Some(&self.cwd),
                    pid: Some(std::process::id()),
                },
                crate::claims::now(),
            );
        }
    }

    /// Leave the session registered on drop -- for a waiter stepping aside
    /// for a newer waiter of the same run and owner, whose registration this
    /// one must not end.
    pub(crate) fn keep_on_drop(&mut self) {
        self.end_on_drop = false;
    }
}

impl Drop for HeadlessSession {
    fn drop(&mut self) {
        if self.end_on_drop
            && let Ok(conn) = crate::db::open()
        {
            let _ = crate::sessions::end(&conn, &self.key, crate::claims::now());
        }
    }
}

#[cfg(test)]
#[path = "claim_liveness_tests.rs"]
mod tests;
