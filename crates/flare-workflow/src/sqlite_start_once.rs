//! Atomic start-once persistence for [`SqliteStore`](super::SqliteStore):
//! durable start receipts committed in one `BEGIN IMMEDIATE` transaction
//! with the run's initial state and input journal entry, plus the atomic
//! read/check/write behind conditional orphan cancellation. Child module of
//! `sqlite_store` (split out for size) so it can reuse the transaction-
//! neutral row writer and the store's locks.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use rusqlite::{OptionalExtension, TransactionBehavior, params};

use super::{SqliteStore, blocking};
use crate::journal;
use crate::types::{
    JournalEntry, StartOnceResult, WorkflowData, WorkflowError, WorkflowResult, WorkflowRunId,
    WorkflowState, WorkflowStatus,
};

/// Store-level outcome of a conditional orphan-cancellation check. The engine
/// maps this to the public [`OrphanCancelDisposition`](crate::OrphanCancelDisposition).
pub(crate) enum OrphanCancelOutcome {
    Cancelled,
    Unchanged,
    Terminal(WorkflowStatus),
    Missing,
}

impl<D: WorkflowData> SqliteStore<D> {
    /// Look up the durable start receipt for `start_key`.
    pub async fn lookup_start(&self, start_key: &str) -> WorkflowResult<Option<WorkflowRunId>> {
        let conn = Arc::clone(&self.conn);
        let key = start_key.to_string();
        blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            conn.query_row(
                "SELECT run_id FROM workflow_start_receipts WHERE start_key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| WorkflowError::Store(format!("lookup_start: {e}")))?
            .map(|s| {
                s.parse::<WorkflowRunId>()
                    .map_err(|e| WorkflowError::Store(format!("bad receipt run_id: {e}")))
            })
            .transpose()
        })
        .await
    }

    /// Atomically commit a start receipt, the run's initial state (+ child
    /// rows), and its initial input journal entry — or return the pre-existing
    /// receipt's run when this key already started. Runs under the store's
    /// `update_lock` (same serialization as every `update()` writer) and a
    /// SQLite `BEGIN IMMEDIATE` transaction: any pre-commit error rolls back
    /// all three writes together, and a concurrent claimant for the same key
    /// rechecks under the transaction and gets `Existing`.
    pub async fn create_start_once(
        &self,
        start_key: String,
        state: WorkflowState<D>,
        input_entry: JournalEntry,
    ) -> WorkflowResult<StartOnceResult> {
        // Serialize with the other load-mutate-write paths (lease renewals,
        // driver settles) so none of them can interleave this commit.
        let _guard = self.update_lock.lock().await;
        let conn = Arc::clone(&self.conn);
        let run_id = state.run_id;
        blocking(move || {
            let mut conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| WorkflowError::Store(format!("begin start-once tx: {e}")))?;
            // Recheck under the transaction: a concurrent start may have
            // committed while this caller prepared its candidate state.
            let existing: Option<String> = tx
                .query_row(
                    "SELECT run_id FROM workflow_start_receipts WHERE start_key = ?1",
                    params![start_key],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| WorkflowError::Store(format!("recheck receipt: {e}")))?;
            if let Some(s) = existing {
                let id = s
                    .parse::<WorkflowRunId>()
                    .map_err(|e| WorkflowError::Store(format!("bad receipt run_id: {e}")))?;
                // Roll back (nothing written yet) and report the winner.
                tx.rollback()
                    .map_err(|e| WorkflowError::Store(format!("rollback: {e}")))?;
                return Ok(StartOnceResult::Existing(id));
            }
            let now = Utc::now().to_rfc3339();
            tx.execute(
                "INSERT INTO workflow_start_receipts (start_key, run_id, created_at)
                 VALUES (?1, ?2, ?3)",
                params![start_key, run_id.to_string(), now],
            )
            .map_err(|e| WorkflowError::Store(format!("insert receipt: {e}")))?;
            Self::write_state_rows(&tx, &state)?;
            journal::append(&tx, &run_id.to_string(), &input_entry)
                .map_err(|e| WorkflowError::Store(format!("start-once journal: {e}")))?;
            tx.commit()
                .map_err(|e| WorkflowError::Store(format!("commit start-once: {e}")))?;
            Ok(StartOnceResult::Created(run_id))
        })
        .await
    }

    /// Conditional orphan cancellation's atomic read/check/write: recheck the
    /// run's live status and lease under the store's mutation protection
    /// (never the caller's possibly-stale scan snapshot), and only then
    /// decide. Paused runs and fresh leases are never touched; already-
    /// terminal runs report their real outcome; a missing row reports
    /// `Missing`; anything else becomes `Cancelled` with its lease cleared.
    pub(crate) async fn cancel_orphaned_run(
        &self,
        run_id: WorkflowRunId,
        now: DateTime<Utc>,
        lease_ttl: std::time::Duration,
    ) -> WorkflowResult<OrphanCancelOutcome> {
        let _guard = self.update_lock.lock().await;
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let mut conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| WorkflowError::Store(format!("begin orphan-cancel tx: {e}")))?;
            let json: Option<String> = tx
                .query_row(
                    "SELECT state_json FROM workflow_runs WHERE id = ?1",
                    params![run_id.to_string()],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| WorkflowError::Store(format!("orphan-cancel load: {e}")))?;
            let Some(json) = json else {
                tx.rollback()
                    .map_err(|e| WorkflowError::Store(format!("rollback: {e}")))?;
                return Ok(OrphanCancelOutcome::Missing);
            };
            let mut state = Self::deserialize(&json)?;
            if state.status.is_terminal() {
                tx.rollback()
                    .map_err(|e| WorkflowError::Store(format!("rollback: {e}")))?;
                return Ok(OrphanCancelOutcome::Terminal(state.status));
            }
            let lease_fresh = state
                .lease
                .as_ref()
                .is_some_and(|l| l.is_fresh(now, lease_ttl));
            if state.status == WorkflowStatus::Paused || lease_fresh {
                tx.rollback()
                    .map_err(|e| WorkflowError::Store(format!("rollback: {e}")))?;
                return Ok(OrphanCancelOutcome::Unchanged);
            }
            state.status = WorkflowStatus::Cancelled;
            state.lease = None;
            state.updated_at = now;
            state.error = Some(
                "interrupted: executor lease stale or missing; run cancelled as an orphan"
                    .to_string(),
            );
            Self::write_state_rows(&tx, &state)?;
            tx.commit()
                .map_err(|e| WorkflowError::Store(format!("commit orphan-cancel: {e}")))?;
            Ok(OrphanCancelOutcome::Cancelled)
        })
        .await
    }
}
