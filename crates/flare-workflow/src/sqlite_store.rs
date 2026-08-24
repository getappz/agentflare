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
use rusqlite::{Connection, OptionalExtension, params};
use rusqlite_migration::{M, Migrations};

use crate::journal;
use crate::store::StateStore;
use crate::types::{
    JournalEntry, MetricsFilter, StepId, StepMetrics, StepStatus, WorkflowContext, WorkflowData,
    WorkflowError, WorkflowMetrics, WorkflowResult, WorkflowRunId, WorkflowState, WorkflowStatus,
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
    // Serializes `update()`'s load-mutate-write cycle, which is not atomic
    // under the connection mutex alone (it's released between the load and
    // the write so `f` can run outside `blocking`). Without this, two
    // concurrent updates for the same run — e.g. two fan-out branches
    // completing around the same time — can race: both load the same
    // pre-mutation state, and the second write silently overwrites the
    // first's step-state change.
    update_lock: Arc<tokio::sync::Mutex<()>>,
    _marker: PhantomData<D>,
}

impl<D: WorkflowData> SqliteStore<D> {
    pub fn open_file(path: &std::path::Path) -> Result<Self, SqliteStoreError> {
        let conn = db_kit::open_file(path, &Self::migrations())?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            update_lock: Arc::new(tokio::sync::Mutex::new(())),
            _marker: PhantomData,
        })
    }

    pub fn open_memory() -> Result<Self, SqliteStoreError> {
        let conn = db_kit::open_memory(&Self::migrations())?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            update_lock: Arc::new(tokio::sync::Mutex::new(())),
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
            M::up(
                "ALTER TABLE step_state ADD COLUMN duration_ms INTEGER;
                 ALTER TABLE step_state ADD COLUMN input_tokens INTEGER;
                 ALTER TABLE step_state ADD COLUMN output_tokens INTEGER;",
            ),
        ])
    }

    fn serialize(state: &WorkflowState<D>) -> WorkflowResult<String> {
        serde_json::to_string(state)
            .map_err(|e| WorkflowError::Store(format!("serialize state: {e}")))
    }

    fn deserialize(json: &str) -> WorkflowResult<WorkflowState<D>> {
        serde_json::from_str(json)
            .map_err(|e| WorkflowError::Store(format!("deserialize state: {e}")))
    }

    /// Like [`Self::deserialize`], but for multi-row scans (`list_active`,
    /// `list_all`): `workflow_runs` is shared by every `WorkflowEngine<D, _>`
    /// pointed at the same database file, so a row written by a *different*
    /// `D` (e.g. another engine's own pipeline type) is expected to fail to
    /// deserialize as this engine's `D`. Warn and skip that row instead of
    /// failing the whole scan — one foreign-typed row must not make every
    /// other run in the table unreadable.
    fn deserialize_or_skip(id: &str, json: &str) -> Option<WorkflowState<D>> {
        match Self::deserialize(json) {
            Ok(state) => Some(state),
            Err(e) => {
                tracing::warn!(run_id = id, error = %e, "skipping unreadable workflow_runs row (likely a different WorkflowData type sharing this store)");
                None
            }
        }
    }

    /// Write a state row plus its step_state/run_vars projections atomically.
    ///
    /// `step_states`/`variables` are fixed-growing maps within a run's
    /// lifetime (steps are seeded once at start, variables are only ever
    /// inserted/overwritten — see `variables::capture_output`; nothing in
    /// this crate removes entries), so a targeted UPSERT of each row is
    /// always sufficient — no per-write DELETE-all pass is needed.
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

        for (step_id, ss) in &state.step_states {
            tx.execute(
                "INSERT INTO step_state
                   (run_id, step_id, status, attempt, last_error, started_at, completed_at,
                    duration_ms, input_tokens, output_tokens)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                 ON CONFLICT(run_id, step_id) DO UPDATE SET
                   status=excluded.status, attempt=excluded.attempt,
                   last_error=excluded.last_error, started_at=excluded.started_at,
                   completed_at=excluded.completed_at, duration_ms=excluded.duration_ms,
                   input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens",
                params![
                    state.run_id.to_string(),
                    step_id.to_string(),
                    ss.status.status_as_str(),
                    ss.attempt as i64,
                    ss.last_error,
                    ss.started_at.map(|t| t.to_rfc3339()),
                    ss.completed_at.map(|t| t.to_rfc3339()),
                    ss.duration_ms as i64,
                    ss.input_tokens as i64,
                    ss.output_tokens as i64,
                ],
            )
            .map_err(|e| WorkflowError::Store(format!("upsert step_state: {e}")))?;
        }

        for (key, value) in &state.variables {
            tx.execute(
                "INSERT INTO run_vars (run_id, key, value) VALUES (?1, ?2, ?3)
                 ON CONFLICT(run_id, key) DO UPDATE SET value=excluded.value",
                params![state.run_id.to_string(), key, value],
            )
            .map_err(|e| WorkflowError::Store(format!("upsert run_vars: {e}")))?;
        }

        tx.commit()
            .map_err(|e| WorkflowError::Store(format!("commit: {e}")))?;
        Ok(())
    }

    fn delete_state(conn: &Connection, run_id: &str) -> WorkflowResult<()> {
        let tx = conn
            .unchecked_transaction()
            .map_err(|e| WorkflowError::Store(format!("begin delete tx: {e}")))?;
        for (table, id_col) in [
            ("workflow_runs", "id"),
            ("journal", "run_id"),
            ("step_state", "run_id"),
            ("run_vars", "run_id"),
        ] {
            tx.execute(
                &format!("DELETE FROM {table} WHERE {id_col} = ?1"),
                params![run_id],
            )
            .map_err(|e| WorkflowError::Store(format!("delete from {table}: {e}")))?;
        }
        tx.commit()
            .map_err(|e| WorkflowError::Store(format!("commit delete: {e}")))?;
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
            WorkflowStatus::Waiting => "waiting",
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
        "waiting" => WorkflowStatus::Waiting,
        "paused" => WorkflowStatus::Paused,
        "completed" => WorkflowStatus::Completed,
        "failed" => WorkflowStatus::Failed,
        "cancelled" => WorkflowStatus::Cancelled,
        _ => WorkflowStatus::Pending,
    }
}

fn step_status_from_str(s: &str) -> StepStatus {
    match s {
        "running" => StepStatus::Running,
        "succeeded" => StepStatus::Succeeded,
        "failed" => StepStatus::Failed,
        "retrying" => StepStatus::Retrying,
        "skipped" => StepStatus::Skipped,
        _ => StepStatus::Pending,
    }
}

/// Build a `WHERE` clause + bound params for `MetricsFilter` against the
/// `workflow_runs` table, aliased `wr` (joined-to or queried directly).
fn metrics_where(filter: &MetricsFilter) -> (String, Vec<Box<dyn rusqlite::types::ToSql>>) {
    let mut conditions = Vec::new();
    let mut sql_params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(workflow_id) = &filter.workflow_id {
        conditions.push("wr.workflow_id = ?".to_string());
        sql_params.push(Box::new(workflow_id.to_string()));
    }
    if let Some(status) = filter.status {
        conditions.push("wr.status = ?".to_string());
        sql_params.push(Box::new(status.status_as_str()));
    }
    if let Some(since) = filter.since {
        conditions.push("wr.created_at >= ?".to_string());
        sql_params.push(Box::new(since.to_rfc3339()));
    }
    let where_clause = if conditions.is_empty() {
        "1=1".to_string()
    } else {
        conditions.join(" AND ")
    };
    (where_clause, sql_params)
}

/// Run a blocking SQLite closure off the async executor thread. Every trait
/// method below is `async fn` but performs synchronous `Mutex<Connection>`
/// locking + rusqlite I/O; without this, that I/O runs directly on whatever
/// tokio worker thread called in, stalling every other task scheduled on it
/// (fatal on a single-threaded runtime, e.g. a daemon's MCP handler loop).
async fn blocking<F, T>(f: F) -> WorkflowResult<T>
where
    F: FnOnce() -> WorkflowResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| WorkflowError::Store(format!("blocking task panicked: {e}")))?
}

#[async_trait]
impl<D: WorkflowData> StateStore<D> for SqliteStore<D> {
    async fn save(&self, state: WorkflowState<D>) -> WorkflowResult<()> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            Self::write_state(&conn, &state)
        })
        .await
    }

    async fn load(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowState<D>> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            conn.query_row(
                "SELECT state_json FROM workflow_runs WHERE id = ?1",
                params![run_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| WorkflowError::Store(format!("load: {e}")))?
            .ok_or(WorkflowError::NotFound(run_id))
            .and_then(|json| Self::deserialize(&json))
        })
        .await
    }

    async fn update<F>(&self, run_id: WorkflowRunId, f: F) -> WorkflowResult<()>
    where
        F: FnOnce(&mut WorkflowState<D>) + Send,
    {
        // Hold this for the whole load-mutate-write cycle below — see the
        // `update_lock` field doc for why the connection mutex alone isn't
        // enough to make this atomic.
        let _guard = self.update_lock.lock().await;

        // `f` is not `'static` (trait-bound, and callers pass closures that
        // borrow local `&StepDefinition` references), so it can't be moved
        // into `spawn_blocking`. Split the blocking SQLite I/O either side of
        // it instead: blocking load -> apply `f` inline (cheap, no I/O) ->
        // blocking write, of only the owned `state`.
        let conn = Arc::clone(&self.conn);
        let json = blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            conn.query_row(
                "SELECT state_json FROM workflow_runs WHERE id = ?1",
                params![run_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|e| WorkflowError::Store(format!("load for update: {e}")))?
            .ok_or(WorkflowError::NotFound(run_id))
        })
        .await?;

        let mut state = Self::deserialize(&json)?;
        f(&mut state);
        state.updated_at = Utc::now();

        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            Self::write_state(&conn, &state)
        })
        .await
    }

    async fn delete(&self, run_id: WorkflowRunId) -> WorkflowResult<()> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            Self::delete_state(&conn, &run_id.to_string())
        })
        .await
    }

    async fn list_active(&self) -> WorkflowResult<Vec<WorkflowState<D>>> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            let mut stmt = conn
                .prepare(
                    "SELECT id, state_json FROM workflow_runs WHERE status IN ('pending','running','waiting')",
                )
                .map_err(|e| WorkflowError::Store(format!("prepare list_active: {e}")))?;
            let rows = stmt
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))
                .map_err(|e| WorkflowError::Store(format!("list_active: {e}")))?;
            let mut out = Vec::new();
            for row in rows {
                let (id, json) = row.map_err(|e| WorkflowError::Store(format!("row: {e}")))?;
                out.extend(Self::deserialize_or_skip(&id, &json));
            }
            Ok(out)
        })
        .await
    }

    async fn list_all(&self) -> WorkflowResult<Vec<WorkflowState<D>>> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            let mut stmt = conn
                .prepare("SELECT id, state_json FROM workflow_runs")
                .map_err(|e| WorkflowError::Store(format!("prepare list_all: {e}")))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| WorkflowError::Store(format!("list_all: {e}")))?;
            let mut out = Vec::new();
            for row in rows {
                let (id, json) = row.map_err(|e| WorkflowError::Store(format!("row: {e}")))?;
                out.extend(Self::deserialize_or_skip(&id, &json));
            }
            Ok(out)
        })
        .await
    }

    async fn is_cancelled(&self, run_id: WorkflowRunId) -> WorkflowResult<bool> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            conn.query_row(
                "SELECT status = 'cancelled' FROM workflow_runs WHERE id = ?1",
                params![run_id.to_string()],
                |row| row.get::<_, bool>(0),
            )
            .optional()
            .map_err(|e| WorkflowError::Store(format!("is_cancelled: {e}")))?
            .ok_or(WorkflowError::NotFound(run_id))
        })
        .await
    }

    async fn cleanup_old_workflows(&self, ttl: Duration) -> usize {
        let conn = Arc::clone(&self.conn);
        let result = blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            let cutoff = (Utc::now()
                - chrono::Duration::from_std(ttl).unwrap_or(chrono::Duration::hours(1)))
            .to_rfc3339();
            let ids: Vec<String> = {
                let mut stmt = conn
                    .prepare("SELECT id FROM workflow_runs WHERE status NOT IN ('pending','running','waiting','paused') AND updated_at < ?1")
                    .map_err(|e| WorkflowError::Store(format!("prepare cleanup: {e}")))?;
                let rows = stmt
                    .query_map(params![cutoff], |row| row.get::<_, String>(0))
                    .map_err(|e| WorkflowError::Store(format!("query cleanup: {e}")))?;
                rows.filter_map(|r| r.ok()).collect()
            };
            for id in &ids {
                let _ = Self::delete_state(&conn, id);
            }
            Ok(ids.len())
        })
        .await;
        result.unwrap_or_else(|e| {
            tracing::error!(error = ?e, "cleanup_old_workflows failed");
            0
        })
    }

    async fn get_context(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowContext<D>> {
        self.load(run_id).await.map(|s| s.context)
    }

    async fn cleanup_if_terminal(&self, run_id: WorkflowRunId) -> bool {
        let conn = Arc::clone(&self.conn);
        let result = blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            let status: Option<String> = conn
                .query_row(
                    "SELECT status FROM workflow_runs WHERE id = ?1",
                    params![run_id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(|e| WorkflowError::Store(format!("cleanup_if_terminal: {e}")))?;
            let terminal = matches!(
                status.as_deref().map(status_from_str),
                Some(
                    WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled
                )
            );
            if terminal {
                let _ = Self::delete_state(&conn, &run_id.to_string());
            }
            Ok(terminal)
        })
        .await;
        result.unwrap_or_else(|e| {
            tracing::error!(error = ?e, "cleanup_if_terminal failed");
            false
        })
    }

    async fn append_journal(
        &self,
        run_id: WorkflowRunId,
        entry: JournalEntry,
    ) -> WorkflowResult<u64> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            journal::append(&conn, &run_id.to_string(), &entry)
        })
        .await
    }

    async fn journal(&self, run_id: WorkflowRunId) -> WorkflowResult<Vec<JournalEntry>> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            journal::read(&conn, &run_id.to_string())
        })
        .await
    }

    async fn workflow_metrics(&self, filter: MetricsFilter) -> WorkflowResult<WorkflowMetrics> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let conn = conn
                .lock()
                .map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            let (where_clause, sql_params) = metrics_where(&filter);

            let mut counts_by_status = std::collections::HashMap::new();
            {
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT wr.status, COUNT(*) FROM workflow_runs wr
                         WHERE {where_clause} GROUP BY wr.status"
                    ))
                    .map_err(|e| WorkflowError::Store(format!("prepare status counts: {e}")))?;
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(sql_params.iter()), |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                    })
                    .map_err(|e| WorkflowError::Store(format!("status counts: {e}")))?;
                for row in rows {
                    let (status, count) = row.map_err(|e| WorkflowError::Store(format!("row: {e}")))?;
                    counts_by_status.insert(status_from_str(&status), count as u64);
                }
            }

            let avg_duration_ms: Option<f64> = conn
                .query_row(
                    &format!(
                        "SELECT AVG((julianday(wr.updated_at) - julianday(wr.created_at)) * 86400000.0)
                         FROM workflow_runs wr WHERE {where_clause}"
                    ),
                    rusqlite::params_from_iter(sql_params.iter()),
                    |row| row.get(0),
                )
                .map_err(|e| WorkflowError::Store(format!("avg duration: {e}")))?;

            let (total_input_tokens, total_output_tokens): (i64, i64) = conn
                .query_row(
                    &format!(
                        "SELECT COALESCE(SUM(ss.input_tokens), 0), COALESCE(SUM(ss.output_tokens), 0)
                         FROM step_state ss JOIN workflow_runs wr ON wr.id = ss.run_id
                         WHERE {where_clause}"
                    ),
                    rusqlite::params_from_iter(sql_params.iter()),
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|e| WorkflowError::Store(format!("token totals: {e}")))?;

            let mut step_breakdown: std::collections::HashMap<StepId, StepMetrics> =
                std::collections::HashMap::new();
            {
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT ss.step_id, ss.status, COUNT(*)
                         FROM step_state ss JOIN workflow_runs wr ON wr.id = ss.run_id
                         WHERE {where_clause} GROUP BY ss.step_id, ss.status"
                    ))
                    .map_err(|e| WorkflowError::Store(format!("prepare step counts: {e}")))?;
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(sql_params.iter()), |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, i64>(2)?,
                        ))
                    })
                    .map_err(|e| WorkflowError::Store(format!("step counts: {e}")))?;
                for row in rows {
                    let (step_id, status, count) =
                        row.map_err(|e| WorkflowError::Store(format!("row: {e}")))?;
                    step_breakdown
                        .entry(StepId::new(step_id))
                        .or_default()
                        .counts_by_status
                        .insert(step_status_from_str(&status), count as u64);
                }
            }
            {
                let mut stmt = conn
                    .prepare(&format!(
                        "SELECT ss.step_id, AVG(ss.duration_ms)
                         FROM step_state ss JOIN workflow_runs wr ON wr.id = ss.run_id
                         WHERE {where_clause} GROUP BY ss.step_id"
                    ))
                    .map_err(|e| WorkflowError::Store(format!("prepare step avg duration: {e}")))?;
                let rows = stmt
                    .query_map(rusqlite::params_from_iter(sql_params.iter()), |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Option<f64>>(1)?))
                    })
                    .map_err(|e| WorkflowError::Store(format!("step avg duration: {e}")))?;
                for row in rows {
                    let (step_id, avg) =
                        row.map_err(|e| WorkflowError::Store(format!("row: {e}")))?;
                    step_breakdown.entry(StepId::new(step_id)).or_default().avg_duration_ms = avg;
                }
            }

            Ok(WorkflowMetrics {
                counts_by_status,
                avg_duration_ms,
                total_input_tokens: total_input_tokens as u64,
                total_output_tokens: total_output_tokens as u64,
                step_breakdown,
            })
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{JournalEntry, StepId, StepState, WorkflowId, WorkflowRunId};

    #[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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
                started_at: Some(Utc::now()),
                completed_at: Some(Utc::now()),
                ..StepState::default()
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

    /// Concurrent `update()` calls on the same run must not lose mutations —
    /// each increments `value` by 1, so N concurrent updates must land N
    /// increments. Without `update_lock`, two updates can both load the
    /// pre-increment value and the second write clobbers the first.
    #[tokio::test]
    async fn concurrent_updates_on_same_run_do_not_lose_writes() {
        let store = SqliteStore::<TestData>::open_memory().unwrap();
        let original = state(&store).await;

        const N: usize = 20;
        let mut tasks = Vec::with_capacity(N);
        for _ in 0..N {
            let store = store.clone();
            tasks.push(tokio::spawn(async move {
                store
                    .update(original.run_id, |s| s.context.data.value += 1)
                    .await
                    .unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        let loaded = store.load(original.run_id).await.unwrap();
        assert_eq!(loaded.context.data.value, 1 + N as i32);
    }

    #[tokio::test]
    async fn journal_append_and_replay_identical() {
        let store = SqliteStore::<TestData>::open_memory().unwrap();
        let run = WorkflowRunId::new();

        let e1 = JournalEntry::Input {
            value: b"in".to_vec(),
        };
        let e2 = JournalEntry::StepRun {
            step_id: StepId::new("s"),
            attempt: 1,
            result: Some(crate::types::EntryResult::Success(b"out".to_vec())),
        };
        let e3 = JournalEntry::WaitEvent {
            name: "approve".into(),
            result: None,
        };

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

    #[tokio::test]
    async fn delete_removes_state_from_all_tables() {
        let store = SqliteStore::<TestData>::open_memory().unwrap();
        let original = state(&store).await;

        store.delete(original.run_id).await.unwrap();

        let err = store.load(original.run_id).await.unwrap_err();
        assert!(matches!(err, WorkflowError::NotFound(_)));
    }

    /// Boot-time gate (item #164): `crate::store::smoke_test` must pass
    /// against the real on-disk `SqliteStore` schema, not just an in-memory
    /// double, since it's what actually caught nothing for the ~33h
    /// `delete_state`'s wrong column name (item #576) went unnoticed.
    #[tokio::test]
    async fn store_smoke_test_passes_against_the_real_sqlite_schema() {
        let store = SqliteStore::<TestData>::open_memory().unwrap();
        crate::store::smoke_test(&store).await.unwrap();
    }

    /// Item #164's other acceptance criterion, driven through the real
    /// rusqlite code path rather than a trait double: `write_state` only
    /// ever inserts into `workflow_runs` (keyed on `id`) for a smoke test's
    /// empty-steps/empty-vars `WorkflowState`, so `save` succeeds here and
    /// the schema defect — `journal` missing the `run_id` column
    /// `delete_state` unconditionally deletes by, the exact shape of #576's
    /// regression — is only reached by `delete_state`'s real SQL, not
    /// silently swallowed as a save failure.
    #[tokio::test]
    async fn smoke_test_fails_against_a_schema_missing_the_id_run_id_distinction() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
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
             );
             CREATE TABLE journal (
                bad_run_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                entry_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                PRIMARY KEY (bad_run_id, seq)
             );
             CREATE TABLE step_state (
                run_id TEXT NOT NULL,
                step_id TEXT NOT NULL,
                status TEXT NOT NULL,
                attempt INTEGER,
                last_error TEXT,
                started_at TEXT,
                completed_at TEXT,
                duration_ms INTEGER,
                input_tokens INTEGER,
                output_tokens INTEGER,
                PRIMARY KEY (run_id, step_id)
             );
             CREATE TABLE run_vars (
                run_id TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT,
                PRIMARY KEY (run_id, key)
             );",
        )
        .unwrap();

        let store = SqliteStore::<TestData> {
            conn: Arc::new(Mutex::new(conn)),
            update_lock: Arc::new(tokio::sync::Mutex::new(())),
            _marker: PhantomData,
        };

        let err = crate::store::smoke_test(&store).await.unwrap_err();
        assert!(matches!(err, WorkflowError::Store(_)), "got {err:?}");
        assert!(
            err.to_string().contains("journal"),
            "expected the failure to come from delete_state's journal delete, got: {err}"
        );
    }
}
