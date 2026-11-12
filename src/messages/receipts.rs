//! Pipeline custody receipts. An interrupted handoff stays in doubt until its
//! workflow checkpoints the same receipt ID; elapsed time never authorizes replay.
use super::{COLUMNS, MAX_BATCH, Message, row, take_undelivered_before};
use rusqlite::{Connection, Transaction, TransactionBehavior, params};

#[derive(Debug, Clone, serde::Serialize)]
pub(crate) struct DeliveryReceipt {
    pub receipt_id: String,
    pub run_id: String,
    pub message: Message,
    pub state: &'static str,
    pub reserved_at: i64,
    pub received_at: Option<i64>,
}

pub(super) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS agent_message_receipts (
            message_id INTEGER PRIMARY KEY REFERENCES agent_messages(id) ON DELETE CASCADE,
            receipt_id TEXT NOT NULL UNIQUE,
            run_id TEXT NOT NULL,
            recipient TEXT NOT NULL,
            reserved_at INTEGER NOT NULL,
            received_at INTEGER
         );
         CREATE INDEX IF NOT EXISTS idx_agent_message_receipts_run
             ON agent_message_receipts(run_id, received_at);",
    )
}

fn receipt(r: &rusqlite::Row<'_>) -> rusqlite::Result<DeliveryReceipt> {
    let received_at: Option<i64> = r.get("received_at")?;
    Ok(DeliveryReceipt {
        receipt_id: r.get("receipt_id")?,
        run_id: r.get("run_id")?,
        message: row(r)?,
        state: if received_at.is_some() {
            "received"
        } else {
            "in_doubt"
        },
        reserved_at: r.get("reserved_at")?,
        received_at,
    })
}

pub(crate) fn unresolved_for_run(
    conn: &Connection,
    run_id: &str,
) -> rusqlite::Result<Vec<DeliveryReceipt>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS}, r.receipt_id, r.run_id, r.reserved_at, r.received_at
         FROM agent_messages m JOIN agent_message_receipts r ON r.message_id = m.id
         WHERE r.run_id = ?1 AND r.received_at IS NULL ORDER BY m.id"
    ))?;
    stmt.query_map([run_id], receipt)?.collect()
}

pub(crate) fn reserve_for_run(
    conn: &Connection,
    run_id: &str,
    to_key: &str,
    created_before: i64,
    now: i64,
) -> rusqlite::Result<Vec<DeliveryReceipt>> {
    if run_id.is_empty() || to_key.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "run and recipient must be nonempty".into(),
        ));
    }
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let existing = unresolved_for_run(&tx, run_id)?;
    if !existing.is_empty() {
        tx.commit()?;
        return Ok(existing);
    }
    for m in take_undelivered_before(&tx, to_key, MAX_BATCH, created_before, now)? {
        tx.execute(
            "INSERT INTO agent_message_receipts(message_id,receipt_id,run_id,recipient,reserved_at)
             VALUES(?1,?2,?3,?4,?5)",
            params![m.id, format!("{run_id}:{}", m.id), run_id, to_key, now],
        )?;
    }
    let reserved = unresolved_for_run(&tx, run_id)?;
    tx.commit()?;
    Ok(reserved)
}

pub(crate) fn confirm(
    conn: &Connection,
    receipt_id: &str,
    run_id: &str,
    recipient: &str,
    now: i64,
) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE agent_message_receipts SET received_at = ?4
         WHERE receipt_id = ?1 AND run_id = ?2 AND recipient = ?3 AND received_at IS NULL",
        params![receipt_id, run_id, recipient, now],
    )
}

pub(crate) fn list(
    conn: &Connection,
    to_key: &str,
    limit: usize,
) -> rusqlite::Result<Vec<DeliveryReceipt>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS}, r.receipt_id, r.run_id, r.reserved_at, r.received_at
         FROM agent_messages m JOIN agent_message_receipts r ON r.message_id = m.id
         WHERE r.recipient = ?1 ORDER BY m.id DESC LIMIT ?2"
    ))?;
    stmt.query_map(params![to_key, limit as i64], receipt)?
        .collect()
}
