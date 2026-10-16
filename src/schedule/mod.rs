//! Durable interval scheduling for project-local JSON workflows (item #366
//! Phase 1, PR-B): `schedules` / `schedule_runs` / `workflow_start_receipts`
//! storage, a daemon-owned 60 s ticker, and the eager terminal-outcome hook.
//! CLI/MCP surfaces (PR-C) enqueue and administer here; only the daemon's
//! `Scheduler` executes.

// The admin/CRUD half of this subtree (schedule CRUD, validation, manual
// receipts) is PR-C (#370) CLI/MCP surface: it ships here with full test
// coverage so the daemon half can build on it, and lights up in production
// when PR-C wires the commands. Silence `dead_code` for the subtree until
// then — PR-C removes this once the commands land — rather than scattering
// per-item allows.
#![allow(dead_code)]

pub(crate) mod store;
pub(crate) mod tick;

pub use store::ScheduleStore;

#[cfg(test)]
mod tests;
#[cfg(test)]
#[path = "tests_lifecycle.rs"]
mod tests_lifecycle;
#[cfg(test)]
#[path = "tests_recovery.rs"]
mod tests_recovery;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use flare_workflow::json::{PipelineData, SendMessage};
use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::{StateStore, WorkflowEngine, WorkflowRunId};

/// Seconds between daemon scheduler ticks.
pub const TICK_INTERVAL_SECS: u64 = 60;
/// Default grace when a schedule sets none: `min(every_secs, 12 h)`.
pub const MAX_GRACE_DEFAULT: i64 = 43200;
/// How long a claimed-but-unlinked dispatch waits for its start receipt
/// before recovery requeues it. A retry threshold only — never proof the
/// start did not happen (the receipt check decides that).
pub const START_RECOVERY_DELAY_SECS: i64 = 120;
/// Per-schedule retention: newest N non-skip terminal occurrences…
pub const RETENTION_NON_SKIP: i64 = 100;
/// …plus, under a separate cap, newest N skip rows.
pub const RETENTION_SKIP: i64 = 100;
/// Minimum schedule interval: the ticker only runs every 60 s.
pub const MIN_EVERY_SECS: i64 = 60;

/// Terminal statuses that are real outcomes (not skips).
pub const TERMINAL_NON_SKIP_STATUSES: [&str; 6] = [
    "completed",
    "failed",
    "cancelled",
    "interrupted",
    "evicted",
    "dispatch_failed",
];
/// Terminal skip statuses, retained under their own cap.
pub const SKIP_STATUSES: [&str; 2] = ["skipped_missed", "skipped_overlap"];

/// One `schedules` row. Timestamps are UTC epoch seconds; `params_raw` is
/// the stored JSON object (None = absent at create).
#[derive(Debug, Clone)]
pub struct ScheduleRow {
    pub id: String,
    pub name: String,
    pub repo_root: String,
    pub workflow_name: String,
    pub params_raw: Option<String>,
    pub every_secs: i64,
    pub grace_secs: Option<i64>,
    pub overlap: String,
    pub enabled: bool,
    pub next_fire_at: i64,
    pub last_fire_at: Option<i64>,
    pub frozen_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl ScheduleRow {
    /// Stored params as JSON: absent means `{}`, anything else must already
    /// be an object (enforced at create/update; a garbage row freezes the
    /// schedule instead of failing the tick).
    pub fn params_json(&self) -> Result<serde_json::Value, String> {
        match &self.params_raw {
            None => Ok(serde_json::json!({})),
            Some(raw) => {
                let v: serde_json::Value =
                    serde_json::from_str(raw).map_err(|e| format!("invalid params JSON: {e}"))?;
                if !v.is_object() {
                    return Err("params must be a JSON object".to_string());
                }
                Ok(v)
            }
        }
    }

    /// Effective grace: explicit `grace_secs`, else `min(every_secs, 12 h)`.
    pub fn effective_grace_secs(&self) -> i64 {
        self.grace_secs
            .unwrap_or_else(|| self.every_secs.min(MAX_GRACE_DEFAULT))
    }
}

/// One `schedule_runs` row.
#[derive(Debug, Clone)]
pub struct OccurrenceRow {
    pub id: String,
    pub schedule_id: String,
    pub slot: i64,
    pub trigger: String,
    pub status: String,
    pub launch_spec: Option<String>,
    pub start_deadline_at: Option<i64>,
    pub dispatch_started_at: Option<i64>,
    pub run_id: Option<String>,
    pub error: Option<String>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
}

impl OccurrenceRow {
    pub fn is_terminal(&self) -> bool {
        TERMINAL_NON_SKIP_STATUSES.contains(&self.status.as_str())
            || SKIP_STATUSES.contains(&self.status.as_str())
    }

    pub fn parsed_launch_spec(&self) -> Result<LaunchSpec, String> {
        let raw = self
            .launch_spec
            .as_deref()
            .ok_or_else(|| "occurrence has no launch_spec".to_string())?;
        serde_json::from_str(raw).map_err(|e| format!("invalid launch_spec: {e}"))
    }
}

/// Frozen request snapshot an admitted occurrence executes from: the accepted
/// schedule state at admission time. Workflow file *contents* resolve at
/// execution; everything here is fixed across updates/restarts.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LaunchSpec {
    pub repo_root: String,
    pub workflow_name: String,
    pub params: serde_json::Value,
    pub schedule_name: String,
}

/// Durable manual-fire acceptance: the request is queued, never executed by
/// the client.
#[derive(Debug, Clone)]
pub struct FireReceipt {
    pub occurrence_id: String,
    pub status: String,
}

/// Per-tick accounting for logs and tests.
#[derive(Debug, Default)]
pub struct TickSummary {
    pub admitted: u64,
    pub skipped_missed: u64,
    pub skipped_overlap: u64,
    pub fired: u64,
    pub reconciled: u64,
    pub pruned: u64,
    pub errors: Vec<String>,
}

/// Durable start key for an occurrence: the receipt is what makes a repeated
/// start return the existing run instead of launching a second driver.
pub fn start_key_for(occurrence_id: &str) -> String {
    format!("schedule:{occurrence_id}")
}

/// `^[A-Za-z0-9._-]{1,64}$` without a regex dependency.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err(format!(
            "invalid schedule name '{name}': 1-64 characters required"
        ));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
    {
        return Err(format!(
            "invalid schedule name '{name}': only A-Z a-z 0-9 . _ - allowed"
        ));
    }
    Ok(())
}

pub fn validate_every_secs(every_secs: i64) -> Result<(), String> {
    if every_secs < MIN_EVERY_SECS {
        return Err(format!(
            "invalid interval {every_secs}s: minimum is {MIN_EVERY_SECS}s"
        ));
    }
    Ok(())
}

pub fn validate_grace_secs(grace_secs: Option<i64>) -> Result<(), String> {
    if grace_secs.is_some_and(|g| g < 0) {
        return Err("invalid grace: must be >= 0".to_string());
    }
    Ok(())
}

pub fn validate_overlap(overlap: &str) -> Result<(), String> {
    if overlap != "skip" && overlap != "allow" {
        return Err(format!(
            "invalid overlap '{overlap}': must be 'skip' or 'allow'"
        ));
    }
    Ok(())
}

/// Params must be absent or a JSON object, and must not claim the reserved
/// top-level `schedule` key the fire path injects.
pub fn validate_params(params: &serde_json::Value) -> Result<(), String> {
    let obj = params
        .as_object()
        .ok_or_else(|| "params must be a JSON object".to_string())?;
    if obj.contains_key("schedule") {
        return Err("params reserves top-level key `schedule`".to_string());
    }
    Ok(())
}

/// Canonicalize a repo root for storage/comparison; reject nonexistent roots.
pub fn canonical_repo_root(root: &str) -> Result<String, String> {
    let path = std::path::Path::new(root);
    if !path.is_dir() {
        return Err(format!("repo root does not exist: '{root}'"));
    }
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| format!("cannot canonicalize repo root '{root}': {e}"))
}

/// Daemon-owned scheduler: one long-lived engine/store pair, per-schedule
/// register-plus-start locks, an in-flight occurrence set, and a tick mutex.
/// Construction registers the eager terminal-outcome callback on the engine
/// so driver completion writes `schedule_runs` directly (tick reconciliation
/// is the crash fallback, not the primary path).
pub struct Scheduler {
    engine: WorkflowEngine<PipelineData, SqliteStore<PipelineData>>,
    store: ScheduleStore,
    send: SendMessage,
    schedule_locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
    in_flight: Arc<Mutex<HashSet<String>>>,
    tick_lock: tokio::sync::Mutex<()>,
    fires: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl Scheduler {
    pub fn new(
        store: ScheduleStore,
        engine: WorkflowEngine<PipelineData, SqliteStore<PipelineData>>,
        send: SendMessage,
    ) -> Self {
        let sched = Self {
            engine,
            store: store.clone(),
            send,
            schedule_locks: Arc::new(Mutex::new(HashMap::new())),
            in_flight: Arc::new(Mutex::new(HashSet::new())),
            tick_lock: tokio::sync::Mutex::new(()),
            fires: Mutex::new(Vec::new()),
        };
        // Eager path: driver completion lands in `schedule_runs` before any
        // later eviction can remove the workflow row. Write failures only
        // log; the tick fallback converges them.
        let cb_store = sched.store.clone();
        sched
            .engine
            .on_terminal(Arc::new(move |run_id, status, error| {
                let store = cb_store.clone();
                Box::pin(async move {
                    let outcome = match status {
                        flare_workflow::WorkflowStatus::Completed => "completed",
                        flare_workflow::WorkflowStatus::Failed => "failed",
                        flare_workflow::WorkflowStatus::Cancelled => "cancelled",
                        _ => return,
                    };
                    if let Err(e) = store
                        .complete_linked_run(
                            &run_id.to_string(),
                            outcome,
                            error.as_deref(),
                            Utc::now().timestamp(),
                        )
                        .await
                    {
                        eprintln!("agentflare-schedule: eager outcome write failed: {e}");
                    }
                })
            }));
        sched
    }

    /// Open both halves (schedule tables + workflow store) on `db_path` with
    /// an ambient-runtime engine. Tests use temp paths; production passes
    /// `default_db_path()` with a `WORKFLOW_RT`-pinned engine via `new`.
    pub fn open(db_path: &std::path::Path, send: SendMessage) -> Result<Self, String> {
        let store = ScheduleStore::open(db_path)?;
        let wf_store =
            SqliteStore::open_file(db_path).map_err(|e| format!("open workflow store: {e}"))?;
        let engine = WorkflowEngine::<PipelineData, _>::with_store(wf_store);
        Ok(Self::new(store, engine, send))
    }

    pub fn store(&self) -> &ScheduleStore {
        &self.store
    }

    fn schedule_lock(&self, schedule_id: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.schedule_locks.lock().expect("schedule locks");
        Arc::clone(
            locks
                .entry(schedule_id.to_string())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))),
        )
    }

    fn mark_in_flight(&self, occurrence_id: &str) -> bool {
        self.in_flight
            .lock()
            .expect("in-flight set")
            .insert(occurrence_id.to_string())
    }

    fn unmark_in_flight(&self, occurrence_id: &str) {
        self.in_flight
            .lock()
            .expect("in-flight set")
            .remove(occurrence_id);
    }

    fn is_in_flight(&self, occurrence_id: &str) -> bool {
        self.in_flight
            .lock()
            .expect("in-flight set")
            .contains(occurrence_id)
    }

    /// Drop finished fire tasks so the handle list cannot grow without bound.
    /// Live tasks are untouched — claiming never waits on them.
    fn reap_fires(&self) {
        self.fires
            .lock()
            .expect("fire handles")
            .retain(|h| !h.is_finished());
    }

    /// Await every outstanding fire task. Tests use this after `tick_once`
    /// to converge asynchronously spawned fires without wall-clock sleeps.
    pub async fn drain_fires(&self) {
        let handles: Vec<_> = self.fires.lock().expect("fire handles").drain(..).collect();
        for h in handles {
            let _ = h.await;
        }
    }

    /// Current engine run lookup for tests and the ticker (receipt authority
    /// stays with `lookup_start`; this is diagnostic linkage only).
    pub async fn lookup_run(
        &self,
        run_id: WorkflowRunId,
    ) -> Result<Option<flare_workflow::WorkflowState<PipelineData>>, String> {
        match self.engine.state_store().load(run_id).await {
            Ok(s) => Ok(Some(s)),
            Err(flare_workflow::WorkflowError::NotFound(_)) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn now_timestamp(now: DateTime<Utc>) -> i64 {
        now.timestamp()
    }
}
