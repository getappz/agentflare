//! SQLite-backed `StateStore` on `agentflare-db-kit` conventions.
//!
//! `workflow_runs.state_json` holds the full serialized `WorkflowState`
//! (authoritative); `step_state` and `run_vars` are queryable projections
//! written in the same transaction, so they can never drift. The `journal`
//! table is the append-only durability log.

use std::marker::PhantomData;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use rusqlite_migration::{Migrations, M};

use crate::journal;
use crate::store::StateStore;
use crate::types::{
    JournalEntry, StepStatus, WorkflowContext, WorkflowData, WorkflowError, WorkflowResult,
    WorkflowRunId, WorkflowState, WorkflowStatus,
};

/// Error opening/creating a SQLite store.
#[derive(Debug, thiserror::Error)]
pub enum SqliteStoreError {
    #[error(transparent)]
    Open(#[from] db_kit::open::Error),
}

/// SQLite-backed workflow state store.
#[derive(Clone)]
pub struct SqliteStore<D: WorkflowData> {
    conn: Arc<Mutex<Connection>>,
    _marker: PhantomData<D>,
}

impl<D: WorkflowData> SqliteStore<D> {
    pub fn open_file(path: &std::path::Path) -> Result<Self, SqliteStoreError> {
        let conn = db_kit::open_file(path, &Self::migrations())?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            _marker: PhantomData,
        })
    }

    pub fn open_memory() -> Result<Self, SqliteStoreError> {
        let conn = db_kit::open_memory(&Self::migrations())?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            _marker: PhantomData,
        })
    }

    fn migrations() -> Migrations<'static> {
        Migrations::new(vec![
            M::up(
                "CREATE TABLE workflow_runs (
                    id TEXT PRIMARY KEY,
                    workflow_id TEXT NOT NULL,
                    status TEXT NOT NULL,
                    current_step TEXT,
                    input TEXT,
                    output TEXT,
                    error TEXT,
                    state_json TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );",
            ),
            M::up(
                "CREATE TABLE journal (
                    run_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    entry_type TEXT NOT NULL,
                    payload TEXT NOT NULL,
                    PRIMARY KEY (run_id, seq)
                );",
            ),
            M::up(
                "CREATE TABLE step_state (
                    run_id TEXT NOT NULL,
                    step_id TEXT NOT NULL,
                    status TEXT NOT NULL,
                    attempt INTEGER,
                    last_error TEXT,
                    started_at TEXT,
                    completed_at TEXT,
                    PRIMARY KEY (run_id, step_id)
                );",
            ),
            M::up(
                "CREATE TABLE run_vars (
                    run_id TEXT NOT NULL,
                    key TEXT NOT NULL,
                    value TEXT,
                    PRIMARY KEY (run_id, key)
                );",
            ),
        ])
    }

    fn serialize(state: &WorkflowState<D>) -> WorkflowResult<String> {
        serde_json::to_string(state)
            .map_err(|e| WorkflowError::Store(format!("serialize state: {e}")))
    }

    fn deserialize(json: &str) -> WorkflowResult<WorkflowState<D>> {
        serde_json::from_str(json).map_err(|e| WorkflowError::Store(format!("deserialize state: {e}")))
    }

    /// Write a state row plus its step_state/run_vars projections atomically.
    fn write_state(conn: &Connection, state: &WorkflowState<D>) -> WorkflowResult<()> {
        let json = Self::serialize(state)?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| WorkflowError::Store(format!("begin tx: {e}")))?;

        tx.execute(
            "INSERT INTO workflow_runs
               (id, workflow_id, status, current_step, input, output, error, state_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(id) DO UPDATE SET
               workflow_id=excluded.workflow_id, status=excluded.status,
               current_step=excluded.current_step, input=excluded.input,
               output=excluded.output, error=excluded.error, state_json=excluded.state_json,
               updated_at=excluded.updated_at",
            params![
                state.run_id.to_string(),
                state.workflow_id.to_string(),
                state.status.status_as_str(),
                state.current_step.as_ref().map(|s| s.to_string()),
                state.input,
                state.output,
                state.error,
                json,
                state.created_at.to_rfc3339(),
                state.updated_at.to_rfc3339(),
            ],
        )
        .map_err(|e| WorkflowError::Store(format!("upsert workflow_runs: {e}")))?;

        tx.execute("DELETE FROM step_state WHERE run_id = ?1", params![state.run_id.to_string()])
            .map_err(|e| WorkflowError::Store(format!("clear step_state: {e}")))?;
        for (step_id, ss) in &state.step_states {
            tx.execute(
                "INSERT INTO step_state
                   (run_id, step_id, status, attempt, last_error, started_at, completed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    state.run_id.to_string(),
                    step_id.to_string(),
                    ss.status.status_as_str(),
                    ss.attempt as i64,
                    ss.last_error,
                    ss.started_at.map(|t| t.to_rfc3339()),
                    ss.completed_at.map(|t| t.to_rfc3339()),
                ],
            )
            .map_err(|e| WorkflowError::Store(format!("insert step_state: {e}")))?;
        }

        tx.execute("DELETE FROM run_vars WHERE run_id = ?1", params![state.run_id.to_string()])
            .map_err(|e| WorkflowError::Store(format!("clear run_vars: {e}")))?;
        for (key, value) in &state.variables {
            tx.execute(
                "INSERT INTO run_vars (run_id, key, value) VALUES (?1, ?2, ?3)",
                params![state.run_id.to_string(), key, value],
            )
            .map_err(|e| WorkflowError::Store(format!("insert run_vars: {e}")))?;
        }

        tx.commit().map_err(|e| WorkflowError::Store(format!("commit: {e}")))?;
        Ok(())
    }

    fn delete_state(conn: &Connection, run_id: &str) -> WorkflowResult<()> {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| WorkflowError::Store(format!("begin delete tx: {e}")))?;
        for table in ["workflow_runs", "journal", "step_state", "run_vars"] {
            tx.execute(&format!("DELETE FROM {table} WHERE run_id = ?1"), params![run_id])
                .map_err(|e| WorkflowError::Store(format!("delete from {table}: {e}")))?;
        }
        tx.commit().map_err(|e| WorkflowError::Store(format!("commit delete: {e}")))?;
        Ok(())
    }
}

trait StatusAsStr {
    fn status_as_str(&self) -> &'static str;
}

impl StatusAsStr for WorkflowStatus {
    fn status_as_str(&self) -> &'static str {
        match self {
            WorkflowStatus::Pending => "pending",
            WorkflowStatus::Running => "running",
            WorkflowStatus::Paused => "paused",
            WorkflowStatus::Completed => "completed",
            WorkflowStatus::Failed => "failed",
            WorkflowStatus::Cancelled => "cancelled",
        }
    }
}

impl StatusAsStr for StepStatus {
    fn status_as_str(&self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::Running => "running",
            StepStatus::Succeeded => "succeeded",
            StepStatus::Failed => "failed",
            StepStatus::Retrying => "retrying",
            StepStatus::Skipped => "skipped",
        }
    }
}

fn status_from_str(s: &str) -> WorkflowStatus {
    match s {
        "running" => WorkflowStatus::Running,
        "paused" => WorkflowStatus::Paused,
        "completed" => WorkflowStatus::Completed,
        "failed" => WorkflowStatus::Failed,
        "cancelled" => WorkflowStatus::Cancelled,
        _ => WorkflowStatus::Pending,
    }
}

#[async_trait]
impl<D: WorkflowData> StateStore<D> for SqliteStore<D> {
    async fn save(&self, state: WorkflowState<D>) -> WorkflowResult<()> {
        let conn = self.conn.lock().map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
        Self::write_state(&conn, &state)
    }

    async fn load(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowState<D>> {
        let conn = self.conn.lock().map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
        conn.query_row(
            "SELECT state_json FROM workflow_runs WHERE id = ?1",
            params![run_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(|e| WorkflowError::Store(format!("load: {e}")))?
        .ok_or(WorkflowError::NotFound(run_id))
        .and_then(|json| Self::deserialize(&json))
    }

    async fn update<F>(&self, run_id: WorkflowRunId, f: F) -> WorkflowResult<()>
    where
        F: FnOnce(&mut WorkflowState<D>) + Send,
    {
        let conn = self.conn.lock().map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
        let json = conn
            .query_row(
                "SELECT state_json FROM workflow_runs WHERE id = ?1",
                params![run_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| WorkflowError::Store(format!("load for update: {e}")))?
            .ok_or(WorkflowError::NotFound(run_id))?;
        let mut state = Self::deserialize(&json)?;
        f(&mut state);
        state.updated_at = Utc::now();
        Self::write_state(&conn, &state)
    }

    async fn delete(&self, run_id: WorkflowRunId) -> WorkflowResult<()> {
        let conn = self.conn.lock().map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
        Self::delete_state(&conn, &run_id.to_string())
    }

    async fn list_active(&self) -> WorkflowResult<Vec<WorkflowState<D>>> {
        let conn = self.conn.lock().map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT state_json FROM workflow_runs WHERE status IN ('pending','running')")
            .map_err(|e| WorkflowError::Store(format!("prepare list_active: {e}")))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| WorkflowError::Store(format!("list_active: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            let json = row.map_err(|e| WorkflowError::Store(format!("row: {e}")))?;
            out.push(Self::deserialize(&json)?);
        }
        Ok(out)
    }

    async fn list_all(&self) -> WorkflowResult<Vec<WorkflowState<D>>> {
        let conn = self.conn.lock().map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
        let mut stmt = conn
            .prepare("SELECT state_json FROM workflow_runs")
            .map_err(|e| WorkflowError::Store(format!("prepare list_all: {e}")))?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| WorkflowError::Store(format!("list_all: {e}")))?;
        let mut out = Vec::new();
        for row in rows {
            let json = row.map_err(|e| WorkflowError::Store(format!("row: {e}")))?;
            out.push(Self::deserialize(&json)?);
        }
        Ok(out)
    }

    async fn is_cancelled(&self, run_id: WorkflowRunId) -> WorkflowResult<bool> {
        let conn = self.conn.lock().map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
        conn.query_row(
            "SELECT status = 'cancelled' FROM workflow_runs WHERE id = ?1",
            params![run_id.to_string()],
            |row| row.get::<_, bool>(0),
        )
        .optional()
        .map_err(|e| WorkflowError::Store(format!("is_cancelled: {e}")))?
        .ok_or(WorkflowError::NotFound(run_id))
    }

    async fn cleanup_old_workflows(&self, ttl: Duration) -> usize {
        let conn = self.conn.lock().expect("flare-workflow sqlite lock");
        let cutoff = (Utc::now() - chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::hours(1)))
            .to_rfc3339();
        let ids: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT id FROM workflow_runs WHERE status NOT IN ('pending','running','paused') AND updated_at < ?1")
                .expect("prepare cleanup");
            let rows = stmt
                .query_map(params![cutoff], |row| row.get::<_, String>(0))
                .expect("query cleanup");
            rows.filter_map(|r| r.ok()).collect()
        };
        for id in &ids {
            let _ = Self::delete_state(&conn, id);
        }
        ids.len()
    }

    async fn get_context(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowContext<D>> {
        self.load(run_id).await.map(|s| s.context)
    }

    async fn cleanup_if_terminal(&self, run_id: WorkflowRunId) -> bool {
        let conn = match self.conn.lock() {
            Ok(c) => c,
            Err(_) => return false,
        };
        let status: Option<String> = conn
            .query_row(
                "SELECT status FROM workflow_runs WHERE id = ?1",
                params![run_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten();
        let terminal = matches!(
            status.as_deref().map(status_from_str),
            Some(WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled)
        );
        if terminal {
            let _ = Self::delete_state(&conn, &run_id.to_string());
        }
        terminal
    }

    async fn append_journal(
        &self,
        run_id: WorkflowRunId,
        entry: JournalEntry,
    ) -> WorkflowResult<u64> {
        let conn = self.conn.lock().map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
        journal::append(&conn, &run_id.to_string(), &entry)
    }

    async fn journal(&self, run_id: WorkflowRunId) -> WorkflowResult<Vec<JournalEntry>> {
        let conn = self.conn.lock().map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
        journal::read(&conn, &run_id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{JournalEntry, StepId, StepState, WorkflowId, WorkflowRunId};

    #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
    struct TestData {
        value: i32,
    }

    impl WorkflowData for TestData {
        fn workflow_type() -> &'static str {
            "test"
        }
    }

    async fn state(store: &SqliteStore<TestData>) -> WorkflowState<TestData> {
        let run = WorkflowRunId::new();
        let mut s = WorkflowState::new(run, WorkflowId::new("wf"), TestData { value: 1 });
        s.input = "hello".into();
        s.variables.insert("v".into(), "x".into());
        s.step_states.insert(
            StepId::new("s1"),
            StepState {
                status: StepStatus::Succeeded,
                attempt: 2,
                last_error: None,
                started_at: Some(Utc::now()),
                completed_at: Some(Utc::now()),
            },
        );
        s.status = WorkflowStatus::Running;
        store.save(s.clone()).await.unwrap();
        s
    }

    #[tokio::test]
    async fn save_load_roundtrip() {
        let store = SqliteStore::<TestData>::open_memory().unwrap();
        let original = state(&store).await;

        let loaded = store.load(original.run_id).await.unwrap();
        assert_eq!(loaded.status, WorkflowStatus::Running);
        assert_eq!(loaded.input, "hello");
        assert_eq!(loaded.variables.get("v").map(String::as_str), Some("x"));
        assert_eq!(loaded.step_states.len(), 1);
        assert_eq!(loaded.step_states[&StepId::new("s1")].attempt, 2);
        assert_eq!(loaded.context.data.value, 1);
    }

    #[tokio::test]
    async fn update_roundtrip_persists_mutation() {
        let store = SqliteStore::<TestData>::open_memory().unwrap();
        let original = state(&store).await;

        store
            .update(original.run_id, |s| {
                s.context.data.value = 99;
                s.status = WorkflowStatus::Completed;
            })
            .await
            .unwrap();

        let loaded = store.load(original.run_id).await.unwrap();
        assert_eq!(loaded.context.data.value, 99);
        assert_eq!(loaded.status, WorkflowStatus::Completed);
    }

    #[tokio::test]
    async fn journal_append_and_replay_identical() {
        let store = SqliteStore::<TestData>::open_memory().unwrap();
        let run = WorkflowRunId::new();

        let e1 = JournalEntry::Input { value: b"in".to_vec() };
        let e2 = JournalEntry::StepRun {
            step_id: StepId::new("s"),
            attempt: 1,
            result: Some(crate::types::EntryResult::Success(b"out".to_vec())),
        };
        let e3 = JournalEntry::WaitEvent { name: "approve".into(), result: None };

        assert_eq!(store.append_journal(run, e1.clone()).await.unwrap(), 1);
        assert_eq!(store.append_journal(run, e2.clone()).await.unwrap(), 2);
        assert_eq!(store.append_journal(run, e3.clone()).await.unwrap(), 3);

        let entries = store.journal(run).await.unwrap();
        assert_eq!(entries.len(), 3);
        assert!(entries[0].is_completed());
        assert!(entries[1].is_completed());
        assert!(!entries[2].is_completed());
        assert_eq!(entries[1].entry_type(), "step_run");
    }

    #[tokio::test]
    async fn missing_run_returns_not_found() {
        let store = SqliteStore::<TestData>::open_memory().unwrap();
        let err = store.load(WorkflowRunId::new()).await.unwrap_err();
        assert!(matches!(err, WorkflowError::NotFound(_)));
    }

    #[tokio::test]
    async fn journal_survives_file_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wf.db");

        let run = WorkflowRunId::new();
        let entry = JournalEntry::StepRun {
            step_id: StepId::new("s"),
            attempt: 1,
            result: Some(crate::types::EntryResult::Success(b"out".to_vec())),
        };

        {
            let store = SqliteStore::<TestData>::open_file(&path).unwrap();
            store.append_journal(run, entry.clone()).await.unwrap();
        }
        // Drop store (crash simulation) — reopen the same file.
        let reopened = SqliteStore::<TestData>::open_file(&path).unwrap();
        let entries = reopened.journal(run).await.unwrap();
        assert_eq!(entries, vec![entry]);

        // State too.
        let store = SqliteStore::<TestData>::open_file(&path).unwrap();
        let s = state(&store).await;
        drop(store);
        let reopened = SqliteStore::<TestData>::open_file(&path).unwrap();
        let loaded = reopened.load(s.run_id).await.unwrap();
        assert_eq!(loaded.input, "hello");
    }
}
