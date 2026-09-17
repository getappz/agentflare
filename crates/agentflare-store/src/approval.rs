//! Persistence for the approval gate (#197, child of #131 EPIC).
//!
//! Pending rows must survive a restart — a queued approval is not lost if
//! the process exits before a human decides. `decide` is a conditional
//! `UPDATE ... WHERE decided_at IS NULL` so the channel surface and the
//! terminal surface can race to answer the same request and the first
//! caller to commit wins; the loser's `decide` call gets back `None`.
//!
//! A dedicated module (not `kv`) because the row shape (TTL, decision,
//! post-execution audit columns) doesn't fit the generic key/value table —
//! same rationale as `leases.rs` / `blobs.rs` getting their own modules.

use rusqlite::{Connection, params};

use crate::{Error, Store};

/// A pending (or already-decided) approval row as read back from storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApprovalRow {
    pub request_id: String,
    pub command_class: String,
    /// Stable classifier-derived key (e.g. `"rm"`, `"git:push"`) used to
    /// persist and check "approve always" grants.
    pub command_key: String,
    pub action_summary: String,
    pub command_redacted: String,
    pub origin: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
}

/// A decided, durable audit row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditRow {
    pub request_id: String,
    pub command_class: String,
    pub command_key: String,
    pub action_summary: String,
    pub command_redacted: String,
    pub origin: String,
    pub created_at: i64,
    pub decided_at: i64,
    pub decision: String,
    pub decided_via: Option<String>,
    pub executed_at: Option<i64>,
    pub execution_outcome: Option<String>,
    pub execution_error: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub fn insert_pending(
    store: &Store,
    request_id: &str,
    command_class: &str,
    command_key: &str,
    action_summary: &str,
    command_redacted: &str,
    origin: &str,
    created_at: i64,
    expires_at: Option<i64>,
) -> Result<(), Error> {
    let conn = store.conn();
    conn.execute(
        "INSERT INTO store_pending_approvals
            (request_id, command_class, command_key, action_summary, command_redacted,
             origin, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            request_id,
            command_class,
            command_key,
            action_summary,
            command_redacted,
            origin,
            created_at,
            expires_at,
        ],
    )?;
    Ok(())
}

/// Transition any row whose `expires_at` has passed into a terminal `deny`
/// (if not already decided). Returns the number of rows transitioned.
pub fn expire_stale(store: &Store, now: i64) -> Result<usize, Error> {
    let conn = store.conn();
    expire_stale_conn(&conn, now)
}

fn expire_stale_conn(conn: &Connection, now: i64) -> Result<usize, Error> {
    let updated = conn.execute(
        "UPDATE store_pending_approvals
         SET decided_at = ?1, decision = 'deny', decided_via = 'timeout'
         WHERE decided_at IS NULL
           AND expires_at IS NOT NULL
           AND expires_at <= ?1",
        params![now],
    )?;
    Ok(updated)
}

const PENDING_COLUMNS: &str = "request_id, command_class, command_key, action_summary, command_redacted, origin, created_at, expires_at";

fn row_to_pending(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingApprovalRow> {
    Ok(PendingApprovalRow {
        request_id: row.get(0)?,
        command_class: row.get(1)?,
        command_key: row.get(2)?,
        action_summary: row.get(3)?,
        command_redacted: row.get(4)?,
        origin: row.get(5)?,
        created_at: row.get(6)?,
        expires_at: row.get(7)?,
    })
}

/// List every undecided row, lazily expiring stale ones first. Orphans from
/// a prior launch are included (survive-restart requirement) so a caller
/// can show/dismiss them.
pub fn list_pending(store: &Store, now: i64) -> Result<Vec<PendingApprovalRow>, Error> {
    let conn = store.conn();
    expire_stale_conn(&conn, now)?;
    let mut stmt = conn.prepare(&format!(
        "SELECT {PENDING_COLUMNS} FROM store_pending_approvals
         WHERE decided_at IS NULL
         ORDER BY created_at ASC"
    ))?;
    let rows = stmt.query_map(params![], row_to_pending)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Look up a persisted decision without mutating state. `None` when the row
/// doesn't exist or is still undecided — used to resolve a TTL-vs-decide
/// race: the timeout path re-reads here before writing its own `deny` so a
/// decision that committed in the same instant is honored.
pub fn get_decision(store: &Store, request_id: &str) -> Result<Option<String>, Error> {
    let conn = store.conn();
    let result = conn
        .query_row(
            "SELECT decision FROM store_pending_approvals
             WHERE request_id = ?1 AND decided_at IS NOT NULL",
            params![request_id],
            |row| row.get::<_, String>(0),
        )
        .ok();
    Ok(result)
}

/// Mark a row decided. `via` names the surface that answered first
/// (`"channel"`, `"terminal"`, `"timeout"`, `"allowlist"`). Returns
/// `Some(row)` only when this call actually committed the decision (the row
/// existed and was still undecided) — `None` means it lost the race or the
/// row is unknown, so callers can tell "I won" (and act on the row, e.g.
/// persist an always-allow grant) from "someone else already decided".
pub fn decide(
    store: &Store,
    request_id: &str,
    decision: &str,
    via: &str,
    now: i64,
) -> Result<Option<PendingApprovalRow>, Error> {
    let conn = store.conn();
    expire_stale_conn(&conn, now)?;
    let updated = conn.execute(
        "UPDATE store_pending_approvals
         SET decided_at = ?1, decision = ?2, decided_via = ?3
         WHERE request_id = ?4 AND decided_at IS NULL",
        params![now, decision, via, request_id],
    )?;
    if updated == 0 {
        return Ok(None);
    }
    let row = conn
        .query_row(
            &format!("SELECT {PENDING_COLUMNS} FROM store_pending_approvals WHERE request_id = ?1"),
            params![request_id],
            row_to_pending,
        )
        .ok();
    Ok(row)
}

/// Write-once terminal execution outcome. Requires a prior `decide`
/// (`decided_at IS NOT NULL`) and only ever writes once
/// (`executed_at IS NULL`) — a later retry/cleanup pass cannot rewrite the
/// original audit row.
pub fn record_execution(
    store: &Store,
    request_id: &str,
    outcome: &str,
    error: Option<&str>,
    now: i64,
) -> Result<bool, Error> {
    let conn = store.conn();
    let updated = conn.execute(
        "UPDATE store_pending_approvals
         SET executed_at = ?1, execution_outcome = ?2, execution_error = ?3
         WHERE request_id = ?4 AND decided_at IS NOT NULL AND executed_at IS NULL",
        params![now, outcome, error, request_id],
    )?;
    Ok(updated > 0)
}

pub fn list_recent_decisions(store: &Store, limit: usize) -> Result<Vec<AuditRow>, Error> {
    let limit = limit.clamp(1, 500) as i64;
    let conn = store.conn();
    let mut stmt = conn.prepare(
        "SELECT request_id, command_class, command_key, action_summary, command_redacted, origin,
                created_at, decided_at, decision, decided_via,
                executed_at, execution_outcome, execution_error
         FROM store_pending_approvals
         WHERE decided_at IS NOT NULL AND decision IS NOT NULL
         ORDER BY decided_at DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(AuditRow {
            request_id: row.get(0)?,
            command_class: row.get(1)?,
            command_key: row.get(2)?,
            action_summary: row.get(3)?,
            command_redacted: row.get(4)?,
            origin: row.get(5)?,
            created_at: row.get(6)?,
            decided_at: row.get(7)?,
            decision: row.get(8)?,
            decided_via: row.get(9)?,
            executed_at: row.get(10)?,
            execution_outcome: row.get(11)?,
            execution_error: row.get(12)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

/// Persist an "always allow" grant for a classified command key. Idempotent.
pub fn allowlist_add(store: &Store, command_key: &str, now: i64) -> Result<(), Error> {
    let conn = store.conn();
    conn.execute(
        "INSERT OR IGNORE INTO store_approval_allowlist (command_key, created_at)
         VALUES (?1, ?2)",
        params![command_key, now],
    )?;
    Ok(())
}

pub fn allowlist_contains(store: &Store, command_key: &str) -> Result<bool, Error> {
    let conn = store.conn();
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM store_approval_allowlist WHERE command_key = ?1)",
        params![command_key],
        |row| row.get(0),
    )?;
    Ok(exists)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_memory().unwrap()
    }

    #[test]
    fn insert_then_list_returns_pending_row() {
        let store = store();
        insert_pending(
            &store,
            "req-1",
            "write",
            "rm",
            "rm file.txt",
            "rm file.txt",
            "terminal",
            1_000,
            Some(1_600),
        )
        .unwrap();
        let rows = list_pending(&store, 1_100).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_id, "req-1");
        assert_eq!(rows[0].command_class, "write");
        assert_eq!(rows[0].command_key, "rm");
    }

    #[test]
    fn decide_marks_row_and_excludes_from_pending_list() {
        let store = store();
        insert_pending(
            &store, "req-9", "write", "rm", "s", "cmd", "terminal", 1_000, None,
        )
        .unwrap();
        let decided = decide(&store, "req-9", "approve_once", "terminal", 1_050).unwrap();
        assert_eq!(decided.map(|r| r.request_id), Some("req-9".to_string()));
        let rows = list_pending(&store, 1_100).unwrap();
        assert!(rows.is_empty());
    }

    #[test]
    fn decide_is_race_safe_first_caller_wins() {
        let store = store();
        insert_pending(
            &store, "race", "write", "rm", "s", "cmd", "terminal", 0, None,
        )
        .unwrap();
        assert!(
            decide(&store, "race", "approve_once", "channel", 10)
                .unwrap()
                .is_some()
        );
        // Second decision on the same row must lose the race.
        assert!(
            decide(&store, "race", "deny", "terminal", 11)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            get_decision(&store, "race").unwrap().as_deref(),
            Some("approve_once")
        );
    }

    #[test]
    fn decide_unknown_id_is_noop() {
        let store = store();
        assert!(
            decide(&store, "nope", "deny", "timeout", 0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn decide_returns_the_command_key_for_always_allow_persistence() {
        let store = store();
        insert_pending(
            &store,
            "req-key",
            "install",
            "npm",
            "s",
            "npm install x",
            "terminal",
            0,
            None,
        )
        .unwrap();
        let decided = decide(&store, "req-key", "approve_always", "terminal", 1)
            .unwrap()
            .expect("decided row");
        assert_eq!(decided.command_key, "npm");
    }

    #[test]
    fn list_pending_expires_stale_rows_before_returning() {
        let store = store();
        insert_pending(
            &store,
            "expired",
            "write",
            "rm",
            "s",
            "cmd",
            "terminal",
            0,
            Some(100),
        )
        .unwrap();
        insert_pending(
            &store,
            "active",
            "write",
            "rm",
            "s",
            "cmd",
            "terminal",
            0,
            Some(10_000),
        )
        .unwrap();

        let rows = list_pending(&store, 500).unwrap();
        let ids: Vec<_> = rows.into_iter().map(|r| r.request_id).collect();
        assert_eq!(ids, vec!["active"]);
        assert_eq!(
            get_decision(&store, "expired").unwrap().as_deref(),
            Some("deny")
        );
    }

    #[test]
    fn timeout_path_reads_committed_decision_before_denying() {
        // Simulates the gate's TTL race: a decision commits right as the
        // timeout fires. `decide(..., "timeout")` must lose (return None)
        // and `get_decision` must report the winning approval so the caller
        // honors it instead of denying anyway.
        let store = store();
        insert_pending(
            &store,
            "race2",
            "write",
            "rm",
            "s",
            "cmd",
            "terminal",
            0,
            Some(1_000),
        )
        .unwrap();
        assert!(
            decide(&store, "race2", "approve_once", "channel", 999)
                .unwrap()
                .is_some()
        );
        // Gate's timeout branch tries to deny...
        assert!(
            decide(&store, "race2", "deny", "timeout", 1_000)
                .unwrap()
                .is_none()
        );
        // ...then re-reads and must see the approval, not a deny.
        assert_eq!(
            get_decision(&store, "race2").unwrap().as_deref(),
            Some("approve_once")
        );
    }

    #[test]
    fn record_execution_requires_prior_decision() {
        let store = store();
        insert_pending(
            &store, "req-exec", "write", "rm", "s", "cmd", "terminal", 0, None,
        )
        .unwrap();
        assert!(!record_execution(&store, "req-exec", "success", None, 5).unwrap());
        decide(&store, "req-exec", "approve_once", "terminal", 1).unwrap();
        assert!(record_execution(&store, "req-exec", "success", None, 5).unwrap());
    }

    #[test]
    fn record_execution_is_write_once() {
        let store = store();
        insert_pending(
            &store, "req-idem", "write", "rm", "s", "cmd", "terminal", 0, None,
        )
        .unwrap();
        decide(&store, "req-idem", "approve_once", "terminal", 1).unwrap();
        assert!(record_execution(&store, "req-idem", "success", Some("ok"), 5).unwrap());
        // Second report must be a no-op.
        assert!(!record_execution(&store, "req-idem", "failure", Some("late"), 6).unwrap());
        let rows = list_recent_decisions(&store, 10).unwrap();
        assert_eq!(rows[0].execution_outcome.as_deref(), Some("success"));
        assert_eq!(rows[0].execution_error.as_deref(), Some("ok"));
    }

    #[test]
    fn pending_rows_survive_reopen_of_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        {
            let store = Store::open_file(&path).unwrap();
            insert_pending(
                &store, "survivor", "write", "rm", "s", "cmd", "terminal", 0, None,
            )
            .unwrap();
        }
        // Reopen — a fresh process would do this after a restart.
        let store = Store::open_file(&path).unwrap();
        let rows = list_pending(&store, 0).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].request_id, "survivor");
    }

    #[test]
    fn list_recent_decisions_returns_durable_audit_rows() {
        let store = store();
        insert_pending(&store, "a", "write", "rm", "s", "cmd", "terminal", 0, None).unwrap();
        insert_pending(
            &store, "b", "network", "curl", "s", "cmd", "terminal", 0, None,
        )
        .unwrap();
        decide(&store, "a", "approve_once", "terminal", 1).unwrap();
        decide(&store, "b", "deny", "timeout", 2).unwrap();

        let rows = list_recent_decisions(&store, 10).unwrap();
        assert_eq!(rows.len(), 2);
        assert!(
            rows.iter()
                .any(|r| r.request_id == "a" && r.decision == "approve_once")
        );
        assert!(
            rows.iter()
                .any(|r| r.request_id == "b" && r.decision == "deny")
        );
    }

    #[test]
    fn allowlist_round_trips() {
        let store = store();
        assert!(!allowlist_contains(&store, "rm").unwrap());
        allowlist_add(&store, "rm", 0).unwrap();
        assert!(allowlist_contains(&store, "rm").unwrap());
        // Idempotent re-add.
        allowlist_add(&store, "rm", 0).unwrap();
        assert!(allowlist_contains(&store, "rm").unwrap());
    }

    #[test]
    fn allowlist_survives_reopen_of_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        {
            let store = Store::open_file(&path).unwrap();
            allowlist_add(&store, "npm", 0).unwrap();
        }
        let store = Store::open_file(&path).unwrap();
        assert!(allowlist_contains(&store, "npm").unwrap());
    }
}
