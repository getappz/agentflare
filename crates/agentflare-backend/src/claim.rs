//! Item claim lease — a thin wrapper over agentflare-db-kit's generic
//! `ClaimLedger`, keyed by `item_id`. Pure lease primitive, no item-state
//! knowledge; `item::claim`/`item::claim_done` compose this with
//! `item::update_state` to make claiming actually mean something.
use db_kit::claim::ClaimLedger;
use rusqlite::Connection;

pub use db_kit::claim::Acquire;

const LEDGER: ClaimLedger = ClaimLedger::new("item_claims", &["item_id"]);

pub fn acquire(
    conn: &Connection,
    item_id: &str,
    owner: &str,
    now: i64,
    ttl_secs: i64,
) -> rusqlite::Result<Acquire> {
    LEDGER.acquire(conn, &[item_id], owner, now, ttl_secs)
}

/// Steal-only acquire: takes over an existing done, stale, or already-ours
/// lease on `item_id` exactly like `acquire`, but returns `Ok(None)`
/// instead of creating one when no row exists. For callers finishing work
/// they believe they already hold (`item_done`'s/`item_release`'s
/// abandoned-claim steal): no row means the lease was released since —
/// e.g. the item was reassigned and its new owner released it — and a plain
/// `acquire` would re-mint the claim for a job that has already lost it.
pub fn acquire_if_stale_only(
    conn: &Connection,
    item_id: &str,
    owner: &str,
    now: i64,
    ttl_secs: i64,
) -> rusqlite::Result<Option<Acquire>> {
    LEDGER.acquire_existing(conn, &[item_id], owner, now, ttl_secs)
}

pub fn heartbeat(
    conn: &Connection,
    item_id: &str,
    owner: &str,
    now: i64,
) -> rusqlite::Result<bool> {
    LEDGER.heartbeat(conn, &[item_id], owner, now)
}

pub fn release(conn: &Connection, item_id: &str, owner: &str) -> rusqlite::Result<bool> {
    LEDGER.release(conn, &[item_id], owner)
}

pub fn done(conn: &Connection, item_id: &str, owner: &str, now: i64) -> rusqlite::Result<bool> {
    LEDGER.done(conn, &[item_id], owner, now)
}

pub fn is_owner(conn: &Connection, item_id: &str, owner: &str) -> rusqlite::Result<bool> {
    LEDGER.is_owner(conn, &[item_id], owner)
}

/// Default claim TTL (override via AGENTFLARE_CLAIM_TTL_SECS) -- the single
/// source of truth; the main binary's `claims::ttl_secs()` delegates here.
pub fn default_ttl_secs() -> i64 {
    std::env::var("AGENTFLARE_CLAIM_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1800) as i64
}

/// TTL actually applied to an acquire/steal attempt on `item_id`: normally
/// `requested_ttl_secs` (the caller's active-work TTL, e.g. the item
/// binary's 4h default), but capped to `default_ttl_secs()` (30 min) while
/// the item currently sits in "in_review". Reaching "in_review" means the
/// claim's owner already pushed and its job exited — `mark_in_review`'s
/// lease-stays-held contract only needs to block a second concurrent PR
/// against the same item, not force a legitimate reclaim (a human,
/// `agentflare work`, or the review-sweep's self-repair dispatch) to wait
/// out the full active-work TTL just because nobody's heartbeat has
/// refreshed the lease since the PR went up (item #108).
pub fn effective_ttl_secs(conn: &Connection, item_id: &str, requested_ttl_secs: i64) -> i64 {
    let in_review = crate::item::get(conn, item_id)
        .ok()
        .and_then(|item| crate::state::get(conn, &item.state_id).ok())
        .is_some_and(|s| s.group_name == "in_review");
    if in_review {
        requested_ttl_secs.min(default_ttl_secs())
    } else {
        requested_ttl_secs
    }
}

/// Returns the current owner of an active claim on this item, if any.
/// Includes stale-but-not-done claims so stale locks can be cleaned up.
pub fn current_owner(conn: &Connection, item_id: &str) -> Option<String> {
    let now = db_kit::ids::now();
    let ttl = default_ttl_secs();
    LEDGER
        .list(conn, true, now, ttl)
        .ok()
        .into_iter()
        .flatten()
        .find(|c| c.key == [item_id] && c.status == "claimed")
        .map(|c| c.owner)
}

/// A live (non-stale) `claimed` lease on an item, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveClaimOnItem {
    pub owner: String,
    pub age_secs: i64,
}

/// Returns the live claim holder on `item_id`, if one exists.
pub fn live_claim_on_item(
    conn: &Connection,
    item_id: &str,
    now: i64,
    ttl_secs: i64,
) -> rusqlite::Result<Option<LiveClaimOnItem>> {
    let claims = LEDGER.list(conn, false, now, ttl_secs)?;
    Ok(claims
        .iter()
        .find(|c| c.key == [item_id])
        .map(|c| LiveClaimOnItem {
            owner: c.owner.clone(),
            age_secs: now - c.heartbeat_at,
        }))
}

/// Returns true if there is an active (live, non-stale) claim on this item
/// whose owner differs from `owner`. Used by the comment edit/delete gates
/// to prevent modifying a comment when another agent has started work.
pub fn has_active_claim_by_other(
    conn: &Connection,
    item_id: &str,
    owner: &str,
    now: i64,
    ttl_secs: i64,
) -> rusqlite::Result<bool> {
    let claims = LEDGER.list(conn, false, now, ttl_secs)?;
    Ok(claims
        .iter()
        .any(|c| c.key == [item_id] && c.owner != owner))
}

/// Returns the item IDs (keys) of all active, non-stale claims.
pub fn list_active(conn: &Connection, now: i64, ttl_secs: i64) -> rusqlite::Result<Vec<String>> {
    let claims = LEDGER.list(conn, false, now, ttl_secs)?;
    Ok(claims
        .into_iter()
        .filter(|c| c.key.len() == 1)
        .map(|c| c.key.into_iter().next().unwrap())
        .collect())
}

/// All claims on file, live and stale alike — powers `item(list,
/// stale_claim=true)`'s structural filter with one batched query instead of
/// a per-item `current_owner` lookup loop.
pub fn list_all(
    conn: &Connection,
    now: i64,
    ttl_secs: i64,
) -> rusqlite::Result<Vec<db_kit::claim::Claim>> {
    LEDGER.list(conn, true, now, ttl_secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::item::{self, CreateItem};
    use crate::project::{self, CreateProject};
    use crate::workspace::{self, CreateWorkspace};

    const TTL: i64 = 1800;

    fn seed_item(conn: &Connection, suffix: &str) -> String {
        let ws = workspace::create(
            conn,
            CreateWorkspace {
                name: format!("Test{suffix}"),
                slug: format!("test{suffix}"),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = project::create(
            conn,
            CreateProject {
                workspace_id: ws.id,
                name: format!("Test{suffix}"),
                identifier: format!("T{suffix}"),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let state_id = crate::state::list_by_project(conn, &proj.id)
            .unwrap()
            .into_iter()
            .find(|s| s.is_default)
            .unwrap()
            .id;
        item::create(
            conn,
            CreateItem {
                project_id: proj.id,
                state_id,
                name: format!("Item{suffix}"),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
                start_date: None,
                due_date: None,
            },
        )
        .unwrap()
        .id
    }

    #[test]
    fn list_all_reports_stale_flag_per_item_without_hiding_stale_rows() {
        let conn = db::open_in_memory().unwrap();
        let fresh_id = seed_item(&conn, "Fresh");
        let stale_id = seed_item(&conn, "Stale");
        acquire(&conn, &fresh_id, "agent-a", 1_000, TTL).unwrap();
        acquire(&conn, &stale_id, "agent-b", 1_000, TTL).unwrap();

        // Advance past the stale item's TTL, but heartbeat the fresh one so
        // only one of the two claims is actually expired.
        let now = 1_000 + TTL + 1;
        heartbeat(&conn, &fresh_id, "agent-a", now).unwrap();

        let claims = list_all(&conn, now, TTL).unwrap();
        assert_eq!(claims.len(), 2, "list_all must not hide the stale claim");

        let fresh = claims.iter().find(|c| c.key == [fresh_id.clone()]).unwrap();
        let stale = claims.iter().find(|c| c.key == [stale_id.clone()]).unwrap();
        assert!(!fresh.stale);
        assert!(stale.stale);
    }

    #[test]
    fn acquire_if_stale_only_refuses_a_released_claim_but_steals_a_stale_one() {
        let conn = db::open_in_memory().unwrap();
        let item_id = seed_item(&conn, "StealOnly");

        // Claimed by the old job, then reassigned + released by the new
        // owner: the row is gone, and the old job must not get it back.
        acquire(&conn, &item_id, "claude-code:old-job", 1_000, TTL).unwrap();
        assert!(release(&conn, &item_id, "claude-code:old-job").unwrap());
        assert_eq!(
            acquire_if_stale_only(&conn, &item_id, "claude-code:old-job", 1_100, TTL).unwrap(),
            None
        );
        assert!(current_owner(&conn, &item_id).is_none());

        // A live claim by someone else is still Held.
        acquire(&conn, &item_id, "codex:new", 1_200, TTL).unwrap();
        assert!(matches!(
            acquire_if_stale_only(&conn, &item_id, "claude-code:old-job", 1_300, TTL).unwrap(),
            Some(Acquire::Held { ref owner, .. }) if owner == "codex:new"
        ));

        // Abandoned past the TTL — stealable, same as `acquire`.
        assert_eq!(
            acquire_if_stale_only(&conn, &item_id, "claude-code:old-job", 1_200 + TTL + 1, TTL)
                .unwrap(),
            Some(Acquire::Acquired)
        );
        assert_eq!(
            current_owner(&conn, &item_id).as_deref(),
            Some("claude-code:old-job")
        );
    }
}
