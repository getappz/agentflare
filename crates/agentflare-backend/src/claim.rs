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

/// Default claim TTL, mirrored from the main binary's `claims::ttl_secs()`
/// (this lower-level crate can't depend on it) — override via
/// AGENTFLARE_CLAIM_TTL_SECS so the two stay in sync.
pub fn default_ttl_secs() -> i64 {
    std::env::var("AGENTFLARE_CLAIM_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(1800) as i64
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
