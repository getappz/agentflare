//! Durable journal storage: append-only, per-run monotonic sequence.
//!
//! The journal is the durability backbone (Restate-style, design only):
//! every step run, timer and wait is appended as a `JournalEntry` whose
//! result field flips from `None` (pending) to `Some` (completed). Recovery
//! replays the journal and re-executes only pending entries — giving
//! exactly-once step execution across crashes and retries.

use rusqlite::{params, Connection};
use serde_json::Value;

use crate::types::{JournalEntry, WorkflowError, WorkflowResult};

/// Append an entry to a run's journal, returning its sequence number.
/// `seq` is `MAX(seq)+1` computed atomically in the same INSERT.
pub fn append(conn: &Connection, run_id: &str, entry: &JournalEntry) -> WorkflowResult<u64> {
    let payload = serde_json::to_string(entry)
        .map_err(|e| WorkflowError::Journal(format!("serialize entry: {e}")))?;
    conn.execute(
        "INSERT INTO journal (run_id, seq, entry_type, payload)
         VALUES (?1, (SELECT IFNULL(MAX(seq), 0) + 1 FROM journal WHERE run_id = ?1), ?2, ?3)",
        params![run_id, entry.entry_type(), payload],
    )
    .map_err(|e| WorkflowError::Journal(format!("append: {e}")))?;
    next_seq(conn, run_id)
}

/// Read a run's full journal in sequence order.
pub fn read(conn: &Connection, run_id: &str) -> WorkflowResult<Vec<JournalEntry>> {
    let mut stmt = conn
        .prepare("SELECT payload FROM journal WHERE run_id = ?1 ORDER BY seq")
        .map_err(|e| WorkflowError::Journal(format!("prepare read: {e}")))?;
    let rows = stmt
        .query_map(params![run_id], |row| row.get::<_, String>(0))
        .map_err(|e| WorkflowError::Journal(format!("query: {e}")))?;

    let mut entries = Vec::new();
    for row in rows {
        let payload = row.map_err(|e| WorkflowError::Journal(format!("row: {e}")))?;
        let entry: JournalEntry = serde_json::from_str(&payload)
            .map_err(|e| WorkflowError::Journal(format!("deserialize entry: {e}")))?;
        entries.push(entry);
    }
    Ok(entries)
}

/// Number of journal entries for a run.
pub fn count(conn: &Connection, run_id: &str) -> WorkflowResult<u64> {
    conn.query_row(
        "SELECT COUNT(*) FROM journal WHERE run_id = ?1",
        params![run_id],
        |row| row.get::<_, i64>(0),
    )
    .map(|n| n as u64)
    .map_err(|e| WorkflowError::Journal(format!("count: {e}")))
}

/// Highest sequence number already written for a run (0 when empty).
fn next_seq(conn: &Connection, run_id: &str) -> WorkflowResult<u64> {
    let max: Option<i64> = conn
        .query_row(
            "SELECT MAX(seq) FROM journal WHERE run_id = ?1",
            params![run_id],
            |row| row.get(0),
        )
        .map_err(|e| WorkflowError::Journal(format!("max seq: {e}")))?;
    Ok(max.unwrap_or(0) as u64)
}

/// Validate a journal payload blob is a well-formed `JournalEntry`.
pub fn parse_payload(value: &Value) -> WorkflowResult<JournalEntry> {
    serde_json::from_value(value.clone())
        .map_err(|e| WorkflowError::Journal(format!("deserialize entry: {e}")))
}
