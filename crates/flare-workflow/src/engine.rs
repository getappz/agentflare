//! Workflow execution engine: DAG-parallel scheduling with journaled,
//! retried step execution.
//!
//! Ported from SMG `wfaas` engine.rs (Apache-2.0), adapted to the journaled
//! model: every terminal step result is appended to the run's durable journal
//! as a `JournalEntry::StepRun` carrying the serialized context (or a failure
//! code). Completed entries are never re-executed on recovery.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use chrono::Utc;
use parking_lot::RwLock;
use tokio::sync::{mpsc, watch};
use tokio::time::timeout;

use crate::definition::{StepDefinition, WorkflowDefinition};
use crate::events::{EventBus, WorkflowEvent};
use crate::retry::{self, Backoff};
use crate::store::{InMemoryStore, StateStore};
use crate::types::*;
use crate::variables::capture_output;
use crate::waits::WakeAt;

/// What's journaled inside `JournalEntry::Input { value }`: the string
/// pipeline seed plus the structured trigger payload, kept together so the
/// journal remains the single record of "how this run was started" without a
/// second `JournalEntry` variant.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct JournaledInput {
    input: String,
    params: serde_json::Value,
}

/// RAII guard that decrements the active-workflow count on drop.
struct ActiveWorkflowGuard {
    active_workflows: Arc<AtomicUsize>,
}

impl Drop for ActiveWorkflowGuard {
    fn drop(&mut self) {
        self.active_workflows.fetch_sub(1, Ordering::Release);
    }
}

/// RAII guard for `start_workflow`; increments on creation, decrements on drop
/// unless `commit()` is called.
struct StartGuard<'a, D: WorkflowData, S: StateStore<D> + 'static> {
    engine: &'a WorkflowEngine<D, S>,
    committed: bool,
}

impl<'a, D: WorkflowData, S: StateStore<D> + 'static> StartGuard<'a, D, S> {
    fn new(engine: &'a WorkflowEngine<D, S>) -> Self {
        engine.active_workflows.fetch_add(1, Ordering::AcqRel);
        Self {
            engine,
            committed: false,
        }
    }

    fn commit(mut self) {
        self.committed = true;
    }
}

impl<D: WorkflowData, S: StateStore<D> + 'static> Drop for StartGuard<'_, D, S> {
    fn drop(&mut self) {
        if !self.committed {
            self.engine.workflow_finished();
        }
    }
}

#[derive(Default)]
struct StepTracker {
    completed: HashSet<StepId>,
    failed: HashSet<StepId>,
    skipped: HashSet<StepId>,
    running: HashSet<StepId>,
    /// Steps waiting for delay/scheduled_at: maps step INDEX to ready time.
    waiting_until: HashMap<usize, Instant>,
}

impl StepTracker {
    fn total_processed(&self) -> usize {
        self.completed.len() + self.failed.len() + self.skipped.len()
    }

    fn is_step_processable(&self, step_id: &StepId, step_idx: usize) -> bool {
        !self.completed.contains(step_id)
            && !self.failed.contains(step_id)
            && !self.skipped.contains(step_id)
            && !self.running.contains(step_id)
            && !self.waiting_until.contains_key(&step_idx)
    }

    fn get_ready_waiting_indices(&self) -> Vec<usize> {
        let now = Instant::now();
        self.waiting_until
            .iter()
            .filter(|&(_, &ready_at)| now >= ready_at)
            .map(|(&idx, _)| idx)
            .collect()
    }

    fn set_waiting(&mut self, step_idx: usize, ready_at: Instant) {
        self.waiting_until.insert(step_idx, ready_at);
    }

    fn clear_waiting(&mut self, step_idx: usize) {
        self.waiting_until.remove(&step_idx);
    }

    fn are_dependencies_satisfied(&self, depends_on: &[StepId]) -> bool {
        depends_on
            .iter()
            .all(|dep| self.completed.contains(dep) || self.skipped.contains(dep))
    }

    fn is_any_dependency_satisfied(&self, depends_on_any: &[StepId]) -> bool {
        depends_on_any.is_empty()
            || depends_on_any
                .iter()
                .any(|dep| self.completed.contains(dep) || self.skipped.contains(dep))
    }

    fn has_failed_dependency(&self, depends_on: &[StepId]) -> bool {
        depends_on.iter().any(|dep| self.failed.contains(dep))
    }

    fn have_all_any_deps_failed(&self, depends_on_any: &[StepId]) -> bool {
        !depends_on_any.is_empty() && depends_on_any.iter().all(|dep| self.failed.contains(dep))
    }
}

/// Out-of-band patch applied to a run's typed data — see
/// [`WorkflowEngine::patch_run_data`].
type DataPatch<D> = Arc<dyn Fn(&mut D) + Send + Sync>;
type DataPatches<D> = Arc<parking_lot::Mutex<HashMap<WorkflowRunId, DataPatch<D>>>>;

/// Default executor-lease TTL: a run whose lease hasn't been renewed for this
/// long is treated as abandoned by a dead process and may be taken over.
pub const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(60);

/// This process's executor identity, `host:pid:nonce`. The random nonce makes
/// it unique even if the OS later reuses the pid (after a reboot, or on a
/// busy host), so a stale lease can never be mistaken for a live one of ours.
fn process_executor_id() -> Arc<str> {
    static ID: std::sync::LazyLock<Arc<str>> = std::sync::LazyLock::new(|| {
        let host = std::env::var("HOSTNAME")
            .or_else(|_| std::env::var("COMPUTERNAME"))
            .unwrap_or_else(|_| "localhost".to_string());
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        Arc::from(format!("{host}:{}:{nonce}", std::process::id()))
    });
    Arc::clone(&ID)
}

/// Whether a process with `pid` currently exists on this host.
fn pid_exists(pid: u32) -> bool {
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        flare_process::command("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }
    #[cfg(windows)]
    {
        flare_process::command("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        true
    }
}

/// Removes a run from its engine's `driving` set (and drops any data patch
/// registered for it) when the task driving it ends, however it ends.
struct DrivingGuard<D: WorkflowData> {
    run_id: WorkflowRunId,
    driving: Arc<parking_lot::Mutex<HashSet<WorkflowRunId>>>,
    data_patches: DataPatches<D>,
}

impl<D: WorkflowData> Drop for DrivingGuard<D> {
    fn drop(&mut self) {
        self.driving.lock().remove(&self.run_id);
        self.data_patches.lock().remove(&self.run_id);
    }
}

/// Aborts the wrapped task when dropped.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Main workflow execution engine.
///
/// `D` is the typed workflow data; `S` is the state store (defaults to
/// in-memory). The engine registers definitions, starts runs, schedules steps
/// against the DAG, retries failures, and appends every step result to the
/// durable journal.
pub struct WorkflowEngine<D: WorkflowData, S: StateStore<D> = InMemoryStore<D>> {
    definitions: Arc<RwLock<HashMap<WorkflowId, Arc<WorkflowDefinition<D>>>>>,
    pub(crate) state_store: S,
    pub(crate) event_bus: Arc<EventBus>,
    shutdown_tx: Arc<watch::Sender<bool>>,
    active_workflows: Arc<AtomicUsize>,
    /// Jitter factor applied to retry backoff delays (0.0-1.0).
    pub(crate) jitter: f64,
    /// In-process completions for pending `WaitEvent` steps, keyed by
    /// `"{run_id}:{step_id}:{name}"`. A completed event is also journaled so
    /// it survives restart and pre-delivery races.
    pub(crate) waiters:
        Arc<parking_lot::Mutex<HashMap<String, tokio::sync::oneshot::Sender<EntryResult>>>>,
    /// Runtime to spawn execution/cleanup tasks on. `None` uses the ambient
    /// `tokio::spawn` (whatever runtime called into the engine); callers that
    /// must keep workflow execution off a shared/single-threaded runtime
    /// (e.g. a daemon's MCP handler loop) set this to an isolated runtime via
    /// [`with_runtime_handle`](Self::with_runtime_handle).
    runtime: Option<tokio::runtime::Handle>,
    /// Identity stamped into [`ExecutorLease::owner`] for runs this engine
    /// drives. Process-wide by default (see `process_executor_id`).
    executor_id: Arc<str>,
    /// How long a lease stays valid without renewal; renewed every quarter
    /// of this while a run is being driven.
    lease_ttl: Duration,
    /// Runs this engine instance is executing right now. Guards against
    /// driving the same run twice in one process (e.g. `resume_run` racing
    /// the boot-time `recover()`).
    driving: Arc<parking_lot::Mutex<HashSet<WorkflowRunId>>>,
    /// See [`patch_run_data`](Self::patch_run_data).
    data_patches: DataPatches<D>,
}

impl<D: WorkflowData> WorkflowEngine<D, InMemoryStore<D>> {
    pub fn new() -> Self {
        Self::with_store(InMemoryStore::new())
    }
}

impl<D: WorkflowData, S: StateStore<D> + 'static> WorkflowEngine<D, S> {
    pub fn with_store(state_store: S) -> Self {
        let (shutdown_tx, _) = watch::channel(false);
        Self {
            definitions: Arc::new(RwLock::new(HashMap::new())),
            state_store,
            event_bus: Arc::new(EventBus::new()),
            shutdown_tx: Arc::new(shutdown_tx),
            active_workflows: Arc::new(AtomicUsize::new(0)),
            jitter: 0.0,
            waiters: Arc::new(parking_lot::Mutex::new(HashMap::new())),
            runtime: None,
            executor_id: process_executor_id(),
            lease_ttl: DEFAULT_LEASE_TTL,
            driving: Arc::new(parking_lot::Mutex::new(HashSet::new())),
            data_patches: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        }
    }

    /// Override the executor-lease TTL (default [`DEFAULT_LEASE_TTL`]).
    pub fn with_lease_ttl(mut self, ttl: Duration) -> Self {
        self.lease_ttl = ttl.max(Duration::from_millis(100));
        self
    }

    /// Override this engine's executor identity. Every engine in one process
    /// shares the process identity by default; tests use this to stand in for
    /// a second, independent process sharing the same store.
    pub fn with_executor_id(mut self, id: impl Into<String>) -> Self {
        self.executor_id = Arc::from(id.into());
        self
    }

    /// The identity this engine stamps into the leases of runs it drives.
    pub fn executor_id(&self) -> &str {
        &self.executor_id
    }

    pub fn lease_ttl(&self) -> Duration {
        self.lease_ttl
    }

    /// Whether this engine instance is executing `run_id` right now.
    pub fn is_driving(&self, run_id: WorkflowRunId) -> bool {
        self.driving.lock().contains(&run_id)
    }

    /// Whether `state` is held by a *different*, still-live executor (a
    /// fresh lease stamped by someone other than this engine), i.e. whether
    /// resuming it here would double-run it.
    pub fn is_leased_elsewhere(&self, state: &WorkflowState<D>) -> bool {
        state.lease.as_ref().is_some_and(|lease| {
            *lease.owner != *self.executor_id && lease.is_fresh(Utc::now(), self.lease_ttl)
        })
    }

    /// Whether `lease`'s holder is provably dead: stamped by a different
    /// process on this same host whose pid no longer exists. Lets boot-time
    /// recovery take over a crashed predecessor's runs at once instead of
    /// waiting out the TTL on their last renewal. Anything unprovable (other
    /// host, unparsable owner, pid still present) counts as alive.
    fn lease_holder_is_dead(&self, lease: &ExecutorLease) -> bool {
        let mut mine = self.executor_id.rsplitn(3, ':');
        let mut theirs = lease.owner.rsplitn(3, ':');
        let (Some(_), Some(my_pid), Some(my_host)) = (mine.next(), mine.next(), mine.next()) else {
            return false;
        };
        let (Some(_), Some(pid), Some(host)) = (theirs.next(), theirs.next(), theirs.next()) else {
            return false;
        };
        if host != my_host || pid == my_pid {
            return false;
        }
        pid.parse::<u32>().is_ok_and(|pid| !pid_exists(pid))
    }

    /// [`is_leased_elsewhere`](Self::is_leased_elsewhere), minus leases whose
    /// holder is provably dead (see `lease_holder_is_dead`).
    fn held_by_live_executor(&self, state: &WorkflowState<D>) -> bool {
        self.is_leased_elsewhere(state)
            && !state
                .lease
                .as_ref()
                .is_some_and(|lease| self.lease_holder_is_dead(lease))
    }

    fn new_lease(&self) -> ExecutorLease {
        ExecutorLease {
            owner: self.executor_id.to_string(),
            renewed_at: Utc::now(),
        }
    }

    /// Rebind part of a run's typed data out of band — e.g. hand an
    /// in-flight run to a new claim owner after a restart. The patch is
    /// applied to the persisted context immediately AND re-applied, for as
    /// long as this engine keeps driving the run, to every context a step
    /// loads and every context a step writes back — so a step that was
    /// already executing when the patch landed can't clobber it with its
    /// stale copy on completion. Replaces any earlier patch for the run.
    pub async fn patch_run_data(
        &self,
        run_id: WorkflowRunId,
        patch: impl Fn(&mut D) + Send + Sync + 'static,
    ) -> WorkflowResult<()> {
        let patch: DataPatch<D> = Arc::new(patch);
        self.data_patches.lock().insert(run_id, Arc::clone(&patch));
        self.state_store
            .update(run_id, |s| patch(&mut s.context.data))
            .await
    }

    /// Apply the registered data patch for `run_id` (if any) to `data`.
    pub(crate) fn apply_data_patch(&self, run_id: WorkflowRunId, data: &mut D) {
        if let Some(patch) = self.data_patches.lock().get(&run_id) {
            patch(data);
        }
    }

    /// Set the jitter factor applied to retry backoff delays.
    pub fn with_jitter(mut self, jitter: f64) -> Self {
        self.jitter = jitter.clamp(0.0, 1.0);
        self
    }

    /// Spawn all workflow execution and background tasks on `handle` instead
    /// of the ambient runtime. Use this when the engine is driven from a
    /// runtime that must not block (e.g. a daemon's single-threaded MCP
    /// handler loop) so agent calls and SQLite I/O run on an isolated pool.
    pub fn with_runtime_handle(mut self, handle: tokio::runtime::Handle) -> Self {
        self.runtime = Some(handle);
        self
    }

    /// Spawn `fut` on the configured runtime, or the ambient one if none was set.
    fn spawn<F>(&self, fut: F) -> tokio::task::JoinHandle<F::Output>
    where
        F: std::future::Future + Send + 'static,
        F::Output: Send + 'static,
    {
        match &self.runtime {
            Some(handle) => handle.spawn(fut),
            None => tokio::spawn(fut),
        }
    }

    pub fn is_shutting_down(&self) -> bool {
        *self.shutdown_tx.borrow()
    }

    /// Initiate graceful shutdown: stop accepting new workflows, allow running
    /// ones to complete. Use [`wait_for_shutdown`](Self::wait_for_shutdown).
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
        tracing::info!("Workflow engine shutdown initiated");
    }

    /// Wait for all active workflows to complete within the timeout.
    pub async fn wait_for_shutdown(&self, timeout_duration: Duration) -> bool {
        let start = tokio::time::Instant::now();
        loop {
            let active = self.active_workflows.load(Ordering::Acquire);
            if active == 0 {
                tracing::info!("All workflows completed, shutdown complete");
                return true;
            }
            if start.elapsed() >= timeout_duration {
                tracing::warn!(remaining_workflows = active, "Shutdown timeout reached");
                return false;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Force-cancel all running workflows (after `wait_for_shutdown` timeouts).
    pub async fn force_cancel_all(&self) -> usize {
        let mut cancelled = 0;
        let active_states = match self.state_store.list_active().await {
            Ok(states) => states,
            Err(e) => {
                tracing::error!(error = ?e, "Failed to list active workflows");
                return 0;
            }
        };
        for state in active_states {
            if self.cancel_workflow(state.run_id).await.is_ok() {
                cancelled += 1;
            }
        }
        cancelled
    }

    pub fn active_workflow_count(&self) -> usize {
        self.active_workflows.load(Ordering::Acquire)
    }

    fn workflow_finished(&self) {
        self.active_workflows.fetch_sub(1, Ordering::Release);
    }

    fn active_workflow_guard(&self) -> ActiveWorkflowGuard {
        ActiveWorkflowGuard {
            active_workflows: Arc::clone(&self.active_workflows),
        }
    }

    /// Start a periodic cleanup task for old terminal workflow states.
    pub fn start_cleanup_task(
        &self,
        ttl: Option<Duration>,
        interval: Option<Duration>,
    ) -> tokio::task::JoinHandle<()> {
        let state_store = self.state_store.clone();
        let ttl = ttl.unwrap_or(Duration::from_secs(3600));
        let interval = interval.unwrap_or(Duration::from_secs(300));
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        self.spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {
                        state_store.cleanup_old_workflows(ttl).await;
                    }
                    _ = shutdown_rx.changed() => {
                        tracing::info!("Cleanup task stopping due to shutdown");
                        break;
                    }
                }
            }
        })
    }

    /// Register a workflow definition, validating the DAG once.
    #[must_use = "registration result should be checked"]
    pub fn register_workflow(
        &self,
        mut definition: WorkflowDefinition<D>,
    ) -> Result<(), crate::definition::ValidationError> {
        definition.validate()?;
        let id = definition.id.clone();
        self.definitions.write().insert(id, Arc::new(definition));
        Ok(())
    }

    pub fn event_bus(&self) -> Arc<EventBus> {
        Arc::clone(&self.event_bus)
    }

    pub fn state_store(&self) -> &S {
        &self.state_store
    }

    /// Start a new workflow run from a registered definition.
    #[must_use = "run ID should be stored or awaited"]
    pub async fn start_workflow(
        &self,
        definition_id: WorkflowId,
        data: D,
        input: String,
    ) -> WorkflowResult<WorkflowRunId> {
        self.start_workflow_with_params(definition_id, data, input, serde_json::Value::Null)
            .await
    }

    /// Start a new workflow run with a structured `params` payload alongside
    /// the string `input` pipeline — `{{params.x}}` dotted-path prompt
    /// expansion reads from it. Parallel entrypoint to [`Self::start_workflow`];
    /// existing callers are unaffected.
    #[must_use = "run ID should be stored or awaited"]
    pub async fn start_workflow_with_params(
        &self,
        definition_id: WorkflowId,
        data: D,
        input: String,
        params: serde_json::Value,
    ) -> WorkflowResult<WorkflowRunId> {
        let guard = StartGuard::new(self);
        if self.is_shutting_down() {
            return Err(WorkflowError::ShuttingDown);
        }

        let definition = self
            .definitions
            .read()
            .get(&definition_id)
            .cloned()
            .ok_or_else(|| WorkflowError::DefinitionNotFound(definition_id.clone()))?;

        let run_id = WorkflowRunId::new();
        let mut state = WorkflowState::new(run_id, definition_id.clone(), data);
        state.status = WorkflowStatus::Running;
        state.lease = Some(self.new_lease());
        state.input = input.clone();
        state.context.params = params.clone();

        state.step_states.reserve(definition.steps.len());
        for step in &definition.steps {
            state
                .step_states
                .insert(step.id.clone(), StepState::default());
        }

        self.state_store.save(state).await?;
        let journaled_input = JournaledInput { input, params };
        let value = serde_json::to_vec(&journaled_input)
            .map_err(|e| WorkflowError::Journal(format!("serialize input: {e}")))?;
        self.state_store
            .append_journal(run_id, JournalEntry::Input { value })
            .await?;

        self.evict_old_runs_if_needed().await?;

        self.event_bus
            .publish(WorkflowEvent::WorkflowStarted {
                run_id,
                definition_id,
            })
            .await;

        guard.commit();

        self.driving.lock().insert(run_id);
        self.spawn_driven(
            run_id,
            Arc::clone(&definition),
            self.active_workflow_guard(),
        );

        Ok(run_id)
    }

    /// Drive `run_id` to completion on a spawned task, renewing its executor
    /// lease for as long as the task lives and releasing it afterwards. The
    /// caller must already have inserted `run_id` into `driving` and counted
    /// it in `active_workflows` (`guard` undoes the latter).
    fn spawn_driven(
        &self,
        run_id: WorkflowRunId,
        definition: Arc<WorkflowDefinition<D>>,
        guard: ActiveWorkflowGuard,
    ) {
        let engine = self.clone_for_execution();
        let driving = DrivingGuard {
            run_id,
            driving: Arc::clone(&self.driving),
            data_patches: Arc::clone(&self.data_patches),
        };
        self.spawn(async move {
            let _guard = guard;
            let _driving = driving;
            let renewer =
                AbortOnDrop(engine.spawn(engine.clone_for_execution().renew_lease_loop(run_id)));
            let result = engine.execute_workflow(run_id, definition).await;
            drop(renewer);
            engine.release_lease(run_id).await;
            if let Err(e) = result {
                tracing::error!(run_id = %run_id, error = ?e, "Workflow execution failed");
            }
        });
    }

    /// Re-stamp this engine's lease on `run_id` every quarter-TTL. Stops if
    /// another live executor has taken the run over meanwhile (never steals a
    /// fresh lease back) or the run disappears.
    async fn renew_lease_loop(self, run_id: WorkflowRunId) {
        let interval = self.lease_ttl / 4;
        loop {
            tokio::time::sleep(interval).await;
            let mut lost_to: Option<String> = None;
            let result = self
                .state_store
                .update(run_id, |s| {
                    if self.is_leased_elsewhere(s) {
                        lost_to = s.lease.as_ref().map(|l| l.owner.clone());
                    } else {
                        s.lease = Some(self.new_lease());
                    }
                })
                .await;
            match result {
                Ok(()) => {
                    if let Some(other) = lost_to {
                        tracing::warn!(run_id = %run_id, holder = %other, "Executor lease taken over by another process; no longer renewing it");
                        return;
                    }
                }
                Err(WorkflowError::NotFound(_)) => return,
                Err(e) => {
                    tracing::warn!(run_id = %run_id, error = ?e, "Failed to renew executor lease");
                }
            }
        }
    }

    /// Clear this engine's lease on `run_id` so a non-terminal leftover can
    /// be taken over immediately instead of after the TTL. Best-effort.
    async fn release_lease(&self, run_id: WorkflowRunId) {
        let _ = self
            .state_store
            .update(run_id, |s| {
                if s.lease
                    .as_ref()
                    .is_some_and(|l| *l.owner == *self.executor_id)
                {
                    s.lease = None;
                }
            })
            .await;
    }

    /// Resume every `Running`/`Pending`/`Waiting` run found in the store
    /// after a crash or restart. Recovery replays each run's journal, skips
    /// steps that already completed (memoized), and re-drives the pending
    /// remainder. Returns the set of resumed run IDs.
    ///
    /// A run whose executor lease is still fresh and held by another process
    /// (e.g. a live `agentflare work` CLI driving it) is left alone; one whose
    /// lease went stale (its holder died) is taken over. One bad row never
    /// aborts the sweep: its error is logged and the rest still resume.
    pub async fn recover(&self) -> WorkflowResult<Vec<WorkflowRunId>> {
        self.recover_with(|_| true).await
    }

    /// [`recover`](Self::recover), but only resumes runs for which `filter`
    /// returns `true` — the caller's chance to veto a run using state the
    /// generic engine can't see (e.g. its work item was completed or
    /// cancelled out of band while the process was down). Vetoed runs are
    /// left untouched.
    pub async fn recover_with<F>(&self, filter: F) -> WorkflowResult<Vec<WorkflowRunId>>
    where
        F: Fn(&WorkflowState<D>) -> bool,
    {
        let active = self.state_store.list_active().await?;
        let mut resumed = Vec::new();
        for state in active {
            let run_id = state.run_id;
            let definition = match self.definitions.read().get(&state.workflow_id).cloned() {
                Some(def) => def,
                None => {
                    tracing::warn!(run_id = %run_id, workflow_id = %state.workflow_id, "Cannot recover run: definition not registered");
                    continue;
                }
            };
            if !filter(&state) {
                tracing::info!(run_id = %run_id, "Recovery filter vetoed resuming run");
                continue;
            }
            match self.try_resume(&state, definition).await {
                Ok(true) => resumed.push(run_id),
                Ok(false) => {}
                Err(e) => {
                    tracing::error!(run_id = %run_id, error = ?e, "Failed to recover run; skipping it");
                }
            }
        }
        Ok(resumed)
    }

    /// Take over and resume one non-terminal run, unless this engine is
    /// already driving it or another live process holds its lease. Returns
    /// whether it was resumed here. Used by a caller that finds an existing
    /// run it would otherwise just wait on — if the process that was
    /// executing it died, waiting would never end.
    pub async fn resume_run(&self, run_id: WorkflowRunId) -> WorkflowResult<bool> {
        let state = self.state_store.load(run_id).await?;
        if matches!(
            state.status,
            WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled
        ) {
            return Ok(false);
        }
        let definition = self
            .definitions
            .read()
            .get(&state.workflow_id)
            .cloned()
            .ok_or_else(|| WorkflowError::DefinitionNotFound(state.workflow_id.clone()))?;
        self.try_resume(&state, definition).await
    }

    async fn try_resume(
        &self,
        state: &WorkflowState<D>,
        definition: Arc<WorkflowDefinition<D>>,
    ) -> WorkflowResult<bool> {
        let run_id = state.run_id;
        if self.held_by_live_executor(state) {
            tracing::info!(run_id = %run_id, holder = ?state.lease.as_ref().map(|l| &l.owner), "Run is held by another live executor; not resuming it here");
            return Ok(false);
        }
        if !self.driving.lock().insert(run_id) {
            return Ok(false);
        }
        // Re-check under the store's update (the listing above may be stale)
        // and claim the lease in the same write.
        let mut taken = true;
        let result = self
            .state_store
            .update(run_id, |s| {
                if self.held_by_live_executor(s)
                    || matches!(
                        s.status,
                        WorkflowStatus::Completed
                            | WorkflowStatus::Failed
                            | WorkflowStatus::Cancelled
                    )
                {
                    taken = false;
                    return;
                }
                s.status = WorkflowStatus::Running;
                s.lease = Some(self.new_lease());
            })
            .await;
        if let Err(e) = result {
            self.driving.lock().remove(&run_id);
            return Err(e);
        }
        if !taken {
            self.driving.lock().remove(&run_id);
            return Ok(false);
        }
        self.active_workflows.fetch_add(1, Ordering::AcqRel);
        self.spawn_driven(run_id, definition, self.active_workflow_guard());
        Ok(true)
    }

    /// Maximum number of retained workflow runs. Oldest completed/failed runs
    /// are evicted when this limit is exceeded (OpenFang semantics); runs in
    /// Pending/Running/Paused are never evicted.
    const MAX_RETAINED_RUNS: usize = 200;

    /// Evict oldest completed/failed runs beyond `MAX_RETAINED_RUNS`.
    async fn evict_old_runs_if_needed(&self) -> WorkflowResult<()> {
        let all = self.state_store.list_all().await?;
        if all.len() <= Self::MAX_RETAINED_RUNS {
            return Ok(());
        }
        let mut evictable: Vec<(WorkflowRunId, chrono::DateTime<Utc>)> = all
            .iter()
            .filter(|s| matches!(s.status, WorkflowStatus::Completed | WorkflowStatus::Failed))
            .map(|s| (s.run_id, s.created_at))
            .collect();
        evictable.sort_by_key(|(_, t)| *t);

        let to_remove = all.len() - Self::MAX_RETAINED_RUNS;
        for (id, _) in evictable.into_iter().take(to_remove) {
            tracing::debug!(run_id = %id, "Evicted old workflow run");
            self.state_store.delete(id).await?;
        }
        Ok(())
    }

    /// Calculate how long a step waits based on delay and/or scheduled_at.
    fn calculate_wait_duration(step: &StepDefinition<D>) -> Option<Duration> {
        let now = Utc::now();
        let schedule_wait = step.scheduled_at.and_then(|scheduled_time| {
            if now < scheduled_time {
                (scheduled_time - now).to_std().ok()
            } else {
                None
            }
        });
        match (step.delay, schedule_wait) {
            (Some(delay), Some(schedule)) => Some(delay + schedule),
            (Some(delay), None) => Some(delay),
            (None, Some(schedule)) => Some(schedule),
            (None, None) => None,
        }
    }

    /// Execute a workflow with DAG-based parallel execution (event-driven
    /// readiness, ported from SMG) and journaled step completion.
    async fn execute_workflow(
        &self,
        run_id: WorkflowRunId,
        definition: Arc<WorkflowDefinition<D>>,
    ) -> WorkflowResult<()> {
        let start_time = std::time::Instant::now();
        let step_count = definition.steps.len();

        let tracker: Arc<RwLock<StepTracker>> = Arc::new(RwLock::new(StepTracker::default()));
        // OpenFang-style fan-out/collect buffer shared across parallel tasks.
        let collect_buffer: Arc<parking_lot::Mutex<Vec<String>>> =
            Arc::new(parking_lot::Mutex::new(Vec::new()));
        let (tx, mut rx) = mpsc::channel::<(StepId, StepResult)>(step_count.max(1));

        // Memoize already-completed steps from the durable journal: a step with
        // a completed journal entry is never re-executed on recovery. Completed
        // dependents are scheduled forward so the rest of the DAG proceeds.
        let mut pending_check: VecDeque<usize> = definition
            .get_initial_step_indices()
            .iter()
            .copied()
            .collect();

        {
            let journal = self.state_store.journal(run_id).await?;
            let mut t = tracker.write();
            for step in &definition.steps {
                let memoized = match &step.mode {
                    StepMode::WaitEvent { name, .. } => {
                        // Completed by name (journal entries carry no step id).
                        journal.iter().any(|e| {
                            matches!(
                                e,
                                JournalEntry::WaitEvent { name: n, result: Some(_) } if n == name
                            )
                        })
                    }
                    _ => {
                        // Last entry for this step decides: success -> done,
                        // failure -> failed, else still pending.
                        match journal.iter().rev().find(|e| match e {
                            JournalEntry::StepRun { step_id, .. }
                            | JournalEntry::Sleep { step_id, .. } => step_id == &step.id,
                            _ => false,
                        }) {
                            Some(JournalEntry::StepRun {
                                result: Some(EntryResult::Success(_)),
                                ..
                            })
                            | Some(JournalEntry::Sleep {
                                result: Some(_), ..
                            }) => {
                                t.completed.insert(step.id.clone());
                                true
                            }
                            Some(JournalEntry::StepRun {
                                result: Some(EntryResult::Failure { .. }),
                                ..
                            }) => {
                                t.failed.insert(step.id.clone());
                                true
                            }
                            // Pending entries (result None) are re-run.
                            _ => false,
                        }
                    }
                };
                if memoized {
                    // Schedule dependents of memoized steps forward.
                    for &dep in definition.get_dependent_indices(&step.id) {
                        pending_check.push_back(dep);
                    }
                }
            }
        }

        loop {
            if self.state_store.is_cancelled(run_id).await? {
                self.event_bus
                    .publish(WorkflowEvent::WorkflowCancelled { run_id })
                    .await;
                return Ok(());
            }

            // Phase 0: drain completion signals so dependents are added.
            // Forward on *any* terminal result, not just Success/Skip: a
            // failed step's dependents must still reach Phase 1 so they get
            // an explicit blocked/Skipped status instead of staying Pending
            // forever (see the deps_blocked_indices handling below).
            while let Ok((step_id, _result)) = rx.try_recv() {
                for &dep_idx in definition.get_dependent_indices(&step_id) {
                    pending_check.push_back(dep_idx);
                }
            }

            // Phase 1: check waiting steps + deps-ready steps + blocked detection.
            let (
                newly_ready_from_wait,
                deps_ready_indices,
                deps_blocked_indices,
                total_processed,
                current_running,
                current_waiting,
            ) = {
                let t = tracker.read();
                let wait_ready = t.get_ready_waiting_indices();
                let mut deps_ready = Vec::new();
                let mut deps_blocked = Vec::new();
                for idx in pending_check.drain(..) {
                    let step = &definition.steps[idx];
                    if !t.is_step_processable(&step.id, idx) {
                        continue;
                    }
                    if t.has_failed_dependency(&step.depends_on)
                        || t.have_all_any_deps_failed(&step.depends_on_any)
                    {
                        deps_blocked.push(idx);
                    } else if t.are_dependencies_satisfied(&step.depends_on)
                        && t.is_any_dependency_satisfied(&step.depends_on_any)
                    {
                        deps_ready.push(idx);
                    }
                }
                (
                    wait_ready,
                    deps_ready,
                    deps_blocked,
                    t.total_processed(),
                    t.running.len(),
                    t.waiting_until.len(),
                )
            };

            // A step blocked by a failed dependency never becomes ready and
            // was previously just dropped, leaving its `step_states` status
            // stuck at `Pending` forever. Give it an explicit terminal status
            // and cascade the same treatment to its own dependents.
            if !deps_blocked_indices.is_empty() {
                {
                    let mut t = tracker.write();
                    for &idx in &deps_blocked_indices {
                        t.skipped.insert(definition.steps[idx].id.clone());
                    }
                }
                for &idx in &deps_blocked_indices {
                    let step_id = definition.steps[idx].id.clone();
                    let _ = self
                        .state_store
                        .update(run_id, |s| {
                            if let Some(ss) = s.step_states.get_mut(&step_id) {
                                ss.status = StepStatus::Skipped;
                                ss.last_error =
                                    Some("skipped: upstream dependency failed".to_string());
                                ss.completed_at = Some(Utc::now());
                            }
                        })
                        .await;
                    for &dep_idx in definition.get_dependent_indices(&step_id) {
                        pending_check.push_back(dep_idx);
                    }
                }
            }

            // Phase 2: process waiting/deps-ready, dedup, launch.
            let (ready_to_launch, steps_added_to_waiting) = {
                let now = Instant::now();
                let mut t = tracker.write();
                let mut added_to_waiting = 0usize;

                for &idx in &newly_ready_from_wait {
                    t.clear_waiting(idx);
                }

                let mut seen = HashSet::new();
                let mut ready: Vec<usize> = Vec::new();
                for idx in newly_ready_from_wait {
                    if seen.insert(idx) {
                        ready.push(idx);
                    }
                }

                for idx in deps_ready_indices {
                    let step = &definition.steps[idx];
                    let wait_duration = Self::calculate_wait_duration(step);
                    if let Some(duration) = wait_duration
                        && duration > Duration::ZERO
                    {
                        t.set_waiting(idx, now + duration);
                        added_to_waiting += 1;
                        continue;
                    }
                    if seen.insert(idx) {
                        ready.push(idx);
                    }
                }

                (ready, added_to_waiting)
            };

            if total_processed == step_count {
                break;
            }

            // Deadlock / blocked detection.
            let effective_waiting = current_waiting + steps_added_to_waiting;
            if ready_to_launch.is_empty()
                && current_running == 0
                && effective_waiting == 0
                && pending_check.is_empty()
            {
                let mut drained_completion = false;
                while let Ok((step_id, _result)) = rx.try_recv() {
                    drained_completion = true;
                    for &dep_idx in definition.get_dependent_indices(&step_id) {
                        pending_check.push_back(dep_idx);
                    }
                }
                if drained_completion {
                    continue;
                }

                let failed_step = tracker.read().failed.iter().next().cloned();
                let base_message = if failed_step.is_some() {
                    "Workflow failed due to step dependency failure"
                } else {
                    "Workflow deadlocked: no steps ready and none running"
                };
                let last_error = match &failed_step {
                    Some(step_id) => self.state_store.load(run_id).await.ok().and_then(|s| {
                        s.step_states
                            .get(step_id)
                            .and_then(|ss| ss.last_error.clone())
                    }),
                    None => None,
                };
                let error_message = match last_error {
                    Some(err) => format!("{base_message}: {err}"),
                    None => base_message.to_string(),
                };
                self.finish_workflow_failed(run_id, &definition, failed_step, error_message)
                    .await?;
                return Ok(());
            }

            if !ready_to_launch.is_empty() {
                let mut t = tracker.write();
                for &idx in &ready_to_launch {
                    t.running.insert(definition.steps[idx].id.clone());
                }
            }

            for step_idx in ready_to_launch {
                let step = &definition.steps[step_idx];
                let engine = self.clone_for_execution();
                let def = Arc::clone(&definition);
                let step_id = step.id.clone();
                let tx = tx.clone();
                let tracker = Arc::clone(&tracker);
                let collect_buffer = Arc::clone(&collect_buffer);

                tokio::spawn(async move {
                    let step = &def.steps[step_idx];

                    if let Some(ref condition) = step.run_if {
                        match engine.state_store.get_context(run_id).await {
                            Ok(ctx) => {
                                if !condition(&ctx) {
                                    {
                                        let mut t = tracker.write();
                                        t.running.remove(&step_id);
                                        t.skipped.insert(step_id.clone());
                                        let _ = tx.try_send((step_id.clone(), StepResult::Skip));
                                    }
                                    let _ = engine
                                        .state_store
                                        .update(run_id, |s| {
                                            if let Some(ss) = s.step_states.get_mut(&step_id) {
                                                ss.status = StepStatus::Skipped;
                                            }
                                        })
                                        .await;
                                    return;
                                }
                            }
                            Err(e) => {
                                tracing::error!(step_id = %step_id, error = ?e, "run_if context error, failing step");
                                {
                                    let mut t = tracker.write();
                                    t.running.remove(&step_id);
                                    t.failed.insert(step_id.clone());
                                    let _ = tx.try_send((step_id.clone(), StepResult::Failure));
                                }
                                let _ = engine
                                    .state_store
                                    .update(run_id, |s| {
                                        if let Some(ss) = s.step_states.get_mut(&step_id) {
                                            ss.status = StepStatus::Failed;
                                            ss.last_error =
                                                Some(format!("run_if context error: {e}"));
                                        }
                                    })
                                    .await;
                                return;
                            }
                        }
                    }

                    // Conditional: skip when the current input channel does not
                    // contain the condition substring (case-insensitive).
                    if let StepMode::Conditional { condition } = &step.mode {
                        let current_input = engine
                            .state_store
                            .load(run_id)
                            .await
                            .map(|s| s.input)
                            .unwrap_or_default();
                        if !current_input
                            .to_lowercase()
                            .contains(&condition.to_lowercase())
                        {
                            {
                                let mut t = tracker.write();
                                t.running.remove(&step_id);
                                t.skipped.insert(step_id.clone());
                                let _ = tx.try_send((step_id.clone(), StepResult::Skip));
                            }
                            let _ = engine
                                .state_store
                                .update(run_id, |s| {
                                    if let Some(ss) = s.step_states.get_mut(&step_id) {
                                        ss.status = StepStatus::Skipped;
                                    }
                                })
                                .await;
                            return;
                        }
                    }

                    let result = match &step.mode {
                        // Data-only: join the fan-out buffer into the input
                        // channel, no executor runs.
                        StepMode::Collect => {
                            let joined = {
                                let mut buf = collect_buffer.lock();
                                let joined = buf.join("\n\n---\n\n");
                                buf.clear();
                                joined
                            };
                            engine
                                .state_store
                                .update(run_id, |s| {
                                    s.input = joined.clone();
                                    s.output = Some(joined.clone());
                                    if let Some(ss) = s.step_states.get_mut(&step_id) {
                                        ss.status = StepStatus::Succeeded;
                                        ss.completed_at = Some(Utc::now());
                                    }
                                })
                                .await
                                .map(|_| StepResult::Success)
                        }
                        StepMode::Loop { .. } => engine.execute_loop(run_id, step, &def).await,
                        StepMode::Sleep { duration_secs } => {
                            engine
                                .execute_sleep(run_id, step, WakeAt::Relative(*duration_secs))
                                .await
                        }
                        StepMode::SleepUntil { wake_at } => {
                            engine
                                .execute_sleep(run_id, step, WakeAt::Absolute(*wake_at))
                                .await
                        }
                        StepMode::WaitEvent { name, timeout_secs } => {
                            engine
                                .execute_wait_event(run_id, step, name, *timeout_secs)
                                .await
                        }
                        _ => engine.execute_step_with_retry(run_id, step, &def).await,
                    };

                    // FanOut steps accumulate their output for the Collect.
                    if matches!(result, Ok(StepResult::Success))
                        && matches!(step.mode, StepMode::FanOut)
                        && let Ok(st) = engine.state_store.load(run_id).await
                    {
                        collect_buffer
                            .lock()
                            .push(st.output.clone().unwrap_or_default());
                    }

                    let needs_skip_update = {
                        let mut t = tracker.write();
                        t.running.remove(&step_id);

                        let (sig, needs_update) = match result {
                            Ok(StepResult::Success) => {
                                t.completed.insert(step_id.clone());
                                (StepResult::Success, false)
                            }
                            Ok(StepResult::Skip) => {
                                t.skipped.insert(step_id.clone());
                                (StepResult::Skip, false)
                            }
                            Ok(StepResult::Failure) | Ok(StepResult::Failed(_)) | Err(_) => {
                                match step.on_failure {
                                    FailureAction::FailWorkflow
                                    | FailureAction::RetryIndefinitely => {
                                        t.failed.insert(step_id.clone());
                                        (StepResult::Failure, false)
                                    }
                                    FailureAction::ContinueNextStep => {
                                        t.skipped.insert(step_id.clone());
                                        (StepResult::Skip, true)
                                    }
                                }
                            }
                        };

                        if let Err(e) = tx.try_send((step_id.clone(), sig)) {
                            use mpsc::error::TrySendError;
                            match e {
                                TrySendError::Full(_) => {
                                    tracing::error!(step_id = %step_id, "Channel full sending step completion")
                                }
                                TrySendError::Closed(_) => {
                                    tracing::debug!(step_id = %step_id, "Channel closed, workflow likely cancelled")
                                }
                            }
                        }

                        needs_update
                    };

                    if needs_skip_update {
                        let _ = engine
                            .state_store
                            .update(run_id, |s| {
                                if let Some(ss) = s.step_states.get_mut(&step_id) {
                                    ss.status = StepStatus::Skipped;
                                }
                            })
                            .await;
                    }
                });
            }

            let (has_running, has_waiting) = {
                let t = tracker.read();
                (!t.running.is_empty(), !t.waiting_until.is_empty())
            };

            if has_running {
                if let Some((completed_step_id, _result)) = rx.recv().await {
                    for &dep_idx in definition.get_dependent_indices(&completed_step_id) {
                        pending_check.push_back(dep_idx);
                    }
                }
            } else if has_waiting {
                let sleep_duration = {
                    let t = tracker.read();
                    let now = Instant::now();
                    t.waiting_until
                        .values()
                        .filter_map(|&ready_at| {
                            if ready_at > now {
                                Some(ready_at - now)
                            } else {
                                None
                            }
                        })
                        .min()
                        .unwrap_or(Duration::from_millis(10))
                };
                let capped_sleep = sleep_duration.min(Duration::from_millis(100));
                tokio::time::sleep(capped_sleep).await;
            }
        }

        let failed_step = {
            let t = tracker.read();
            t.failed.iter().next().cloned()
        };

        if let Some(step) = failed_step {
            let last_error = self.state_store.load(run_id).await.ok().and_then(|s| {
                s.step_states
                    .get(&step)
                    .and_then(|ss| ss.last_error.clone())
            });
            let error_message = match last_error {
                Some(err) => format!("One or more steps failed: {err}"),
                None => "One or more steps failed".to_string(),
            };
            self.finish_workflow_failed(run_id, &definition, Some(step), error_message)
                .await?;
        } else {
            let output = self
                .state_store
                .load(run_id)
                .await?
                .output
                .clone()
                .unwrap_or_default();
            self.state_store
                .append_journal(
                    run_id,
                    JournalEntry::Output {
                        result: EntryResult::Success(output.into_bytes()),
                    },
                )
                .await?;
            self.state_store
                .update(run_id, |s| {
                    s.status = WorkflowStatus::Completed;
                })
                .await?;

            let duration = start_time.elapsed();
            self.event_bus
                .publish(WorkflowEvent::WorkflowCompleted { run_id, duration })
                .await;
        }

        Ok(())
    }

    /// Settle a run to `WorkflowStatus::Failed`, running the saga rollback
    /// phase first if any step in `definition` registered a `rollback`
    /// handler. `failed_step` is the specific step whose failure triggered
    /// the workflow's failure, if any (a genuine deadlock with no failed
    /// step passes `None`). Zero overhead for workflows with no registered
    /// rollbacks: the `.any(...)` check short-circuits before any journal
    /// reads.
    async fn finish_workflow_failed(
        &self,
        run_id: WorkflowRunId,
        definition: &WorkflowDefinition<D>,
        failed_step: Option<StepId>,
        error_message: String,
    ) -> WorkflowResult<()> {
        if definition.steps.iter().any(|s| s.rollback.is_some()) {
            self.run_rollback_phase(run_id, definition, failed_step.as_ref())
                .await?;
        }
        // Status flips to `Failed` only here, after the rollback phase
        // returns — a crash mid-unwind leaves the run `Running`, so
        // `recover()` (which only resumes `Running`/`Pending` runs) picks it
        // back up and `run_rollback_phase` resumes from whatever `Rollback`
        // entries already exist.
        self.state_store
            .update(run_id, |s| {
                s.status = WorkflowStatus::Failed;
                // `run_rollback_phase` may have already folded a
                // compensation-failure note into `s.error` above; append the
                // primary failure reason rather than clobbering it.
                s.error = Some(match s.error.take() {
                    Some(rollback_note) => format!("{error_message}; {rollback_note}"),
                    None => error_message.clone(),
                });
            })
            .await?;
        self.event_bus
            .publish(WorkflowEvent::WorkflowFailed {
                run_id,
                failed_step: failed_step.unwrap_or_else(|| StepId::new("internal_scheduler")),
                error: error_message,
            })
            .await;
        Ok(())
    }

    /// Execute a step with retry logic, appending the terminal result to the
    /// durable journal so recovery never re-executes completed steps.
    async fn execute_step_with_retry(
        &self,
        run_id: WorkflowRunId,
        step: &StepDefinition<D>,
        definition: &WorkflowDefinition<D>,
    ) -> WorkflowResult<StepResult> {
        let retry_policy = definition.get_retry_policy(step);
        let step_timeout = definition.get_timeout(step);

        let mut attempt = 1;
        let max_attempts = retry::effective_max_attempts(
            retry_policy.max_attempts,
            matches!(step.on_failure, FailureAction::RetryIndefinitely),
        );
        let mut backoff = Backoff::from_strategy(&retry_policy.backoff);

        loop {
            if self.state_store.is_cancelled(run_id).await? {
                return Err(WorkflowError::Cancelled(run_id));
            }

            self.state_store
                .update(run_id, |s| {
                    s.current_step = Some(step.id.clone());
                    if let Some(ss) = s.step_states.get_mut(&step.id) {
                        ss.status = if attempt == 1 {
                            StepStatus::Running
                        } else {
                            StepStatus::Retrying
                        };
                        ss.attempt = attempt;
                        ss.started_at = Some(Utc::now());
                    }
                })
                .await?;

            self.event_bus
                .publish(WorkflowEvent::StepStarted {
                    run_id,
                    step_id: step.id.clone(),
                    attempt,
                })
                .await;

            let state = self.state_store.load(run_id).await?;
            let mut context = state.context.clone();
            self.apply_data_patch(run_id, &mut context.data);
            context.input = state.input.clone();
            context.variables = state.variables.clone();
            context.step = StepExecutionMeta {
                workflow_id: definition.id.to_string(),
                workflow_name: definition.name.clone(),
                step_id: step.id.to_string(),
                step_name: step.name.clone(),
                attempt,
                max_attempts,
                timeout: step_timeout,
            };
            let step_start = std::time::Instant::now();
            let result = timeout(step_timeout, step.executor.execute(&mut context)).await;
            let step_duration = step_start.elapsed();

            if !matches!(result, Ok(Ok(StepResult::Skip))) {
                self.state_store
                    .update(run_id, |s| {
                        s.context = context.clone();
                        // A patch registered while this step was executing
                        // must survive the step's stale write-back.
                        self.apply_data_patch(run_id, &mut s.context.data);
                    })
                    .await?;
            }

            let context_bytes = serde_json::to_vec(&context)
                .map_err(|e| WorkflowError::Journal(format!("serialize context: {e}")))?;

            match result {
                Ok(Ok(StepResult::Success)) => {
                    self.state_store
                        .update(run_id, |s| {
                            // Chain the string pipeline: step output -> {{input}}.
                            let out = context.output.clone();
                            s.input = out.clone();
                            s.output = Some(out.clone());
                            if let Some(var) = step.output_var.as_deref() {
                                capture_output(&mut s.variables, Some(var), &out);
                                // Keep the persisted context's variables (what
                                // `run_if` reads via `get_context`) in sync
                                // with this step's own just-captured output —
                                // otherwise a downstream step's run_if sees a
                                // one-step-stale snapshot.
                                s.context.variables = s.variables.clone();
                            }
                            if let Some(ss) = s.step_states.get_mut(&step.id) {
                                ss.status = StepStatus::Succeeded;
                                ss.completed_at = Some(Utc::now());
                                ss.input_tokens = context.input_tokens;
                                ss.output_tokens = context.output_tokens;
                                ss.duration_ms = step_duration.as_millis() as u64;
                            }
                        })
                        .await?;
                    self.state_store
                        .append_journal(
                            run_id,
                            JournalEntry::StepRun {
                                step_id: step.id.clone(),
                                attempt,
                                result: Some(EntryResult::Success(context_bytes)),
                            },
                        )
                        .await?;
                    self.event_bus
                        .publish(WorkflowEvent::StepSucceeded {
                            run_id,
                            step_id: step.id.clone(),
                            duration: step_duration,
                        })
                        .await;
                    if let Err(e) = step.executor.on_success(&context).await {
                        tracing::warn!(step_id = %step.id, error = ?e, "on_success hook failed");
                    }
                    return Ok(StepResult::Success);
                }
                Ok(Ok(StepResult::Skip)) => {
                    return Ok(StepResult::Skip);
                }
                Ok(Ok(StepResult::Failure))
                | Ok(Ok(StepResult::Failed(_)))
                | Ok(Err(_))
                | Err(_) => {
                    let (error_msg, should_retry) = match result {
                        Ok(Err(e)) => {
                            let retryable = step.executor.is_retryable(&e);
                            (format!("{e}"), retryable)
                        }
                        Err(_) => (format!("Step timeout after {step_timeout:?}"), true),
                        // Carries its own reason — never retried, same as `Failure`.
                        Ok(Ok(StepResult::Failed(ref msg))) => (msg.clone(), false),
                        _ => ("Step failed".to_string(), false),
                    };

                    let will_retry = should_retry && attempt < max_attempts;

                    self.state_store
                        .update(run_id, |s| {
                            if let Some(ss) = s.step_states.get_mut(&step.id) {
                                ss.status = if will_retry {
                                    StepStatus::Retrying
                                } else {
                                    StepStatus::Failed
                                };
                                ss.last_error = Some(error_msg.clone());
                                if !will_retry {
                                    ss.completed_at = Some(Utc::now());
                                }
                            }
                        })
                        .await?;

                    self.event_bus
                        .publish(WorkflowEvent::StepFailed {
                            run_id,
                            step_id: step.id.clone(),
                            error: error_msg.clone(),
                            will_retry,
                        })
                        .await;

                    if will_retry {
                        let delay = backoff
                            .next(self.jitter)
                            .unwrap_or_else(|| Duration::from_secs(1));
                        self.event_bus
                            .publish(WorkflowEvent::StepRetrying {
                                run_id,
                                step_id: step.id.clone(),
                                attempt: attempt + 1,
                                delay,
                            })
                            .await;
                        tokio::time::sleep(delay).await;
                        attempt += 1;
                    } else {
                        let hook_error = WorkflowError::StepFailed {
                            step_id: step.id.clone(),
                            message: error_msg.clone(),
                        };
                        if let Err(e) = step.executor.on_failure(&context, &hook_error).await {
                            tracing::warn!(step_id = %step.id, error = ?e, "on_failure hook failed");
                        }
                        // Terminal failure is journaled so recovery sees the
                        // step as failed and does not re-run it from scratch.
                        self.state_store
                            .append_journal(
                                run_id,
                                JournalEntry::StepRun {
                                    step_id: step.id.clone(),
                                    attempt,
                                    result: Some(EntryResult::Failure {
                                        code: 1,
                                        message: error_msg.clone(),
                                        metadata: vec![],
                                    }),
                                },
                            )
                            .await?;
                        // ErrorMode::Skip (and FailureAction::ContinueNextStep)
                        // turn a terminal failure into a skip so the workflow
                        // continues.
                        let skip = matches!(step.error_mode, ErrorMode::Skip)
                            || matches!(step.on_failure, FailureAction::ContinueNextStep);
                        return Ok(if skip {
                            StepResult::Skip
                        } else {
                            StepResult::Failure
                        });
                    }
                }
            }
        }
    }

    /// Cancel a running workflow. Already-succeeded steps are left
    /// uncompensated — use [`cancel_workflow_with_rollback`](Self::cancel_workflow_with_rollback)
    /// to run their saga rollback handlers first.
    pub async fn cancel_workflow(&self, run_id: WorkflowRunId) -> WorkflowResult<()> {
        self.cancel_workflow_impl(run_id, false).await
    }

    /// Cancel a running workflow, first running the saga rollback phase for
    /// every already-succeeded step with a registered `rollback` handler —
    /// the cancellation analogue of Cloudflare Workflows'
    /// `instance.terminate({ rollback: true })`. `failed_step` is passed as
    /// `None` to [`run_rollback_phase`](Self::run_rollback_phase) since
    /// cancellation isn't triggered by any particular step.
    pub async fn cancel_workflow_with_rollback(&self, run_id: WorkflowRunId) -> WorkflowResult<()> {
        self.cancel_workflow_impl(run_id, true).await
    }

    async fn cancel_workflow_impl(
        &self,
        run_id: WorkflowRunId,
        rollback: bool,
    ) -> WorkflowResult<()> {
        if rollback {
            let state = self.state_store.load(run_id).await?;
            let definition = self.definitions.read().get(&state.workflow_id).cloned();
            if let Some(definition) = definition {
                if definition.steps.iter().any(|s| s.rollback.is_some()) {
                    self.run_rollback_phase(run_id, &definition, None).await?;
                }
            } else {
                tracing::warn!(run_id = %run_id, workflow_id = %state.workflow_id, "cancel_workflow_with_rollback: definition not registered, skipping rollback phase");
            }
        }
        self.state_store
            .update(run_id, |s| s.status = WorkflowStatus::Cancelled)
            .await?;
        self.event_bus
            .publish(WorkflowEvent::WorkflowCancelled { run_id })
            .await;
        Ok(())
    }

    /// Get workflow status.
    pub async fn get_status(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowState<D>> {
        self.state_store.load(run_id).await
    }

    /// Aggregate instance/step metrics over runs matching `filter`.
    pub async fn metrics(&self, filter: MetricsFilter) -> WorkflowResult<WorkflowMetrics> {
        self.state_store.workflow_metrics(filter).await
    }

    /// Wait for a workflow to complete with adaptive polling.
    pub async fn wait_for_completion(
        &self,
        run_id: WorkflowRunId,
        label: &str,
        timeout_duration: Duration,
    ) -> Result<String, String> {
        let start = std::time::Instant::now();
        let mut poll_interval = Duration::from_millis(100);
        let max_poll_interval = Duration::from_millis(2000);
        let poll_backoff = Duration::from_millis(200);

        loop {
            if start.elapsed() > timeout_duration {
                return Err(format!(
                    "Workflow timeout after {}s for {label}",
                    timeout_duration.as_secs()
                ));
            }

            let state = self
                .get_status(run_id)
                .await
                .map_err(|e| format!("Failed to get status: {e:?}"))?;

            let result = match state.status {
                WorkflowStatus::Completed => {
                    Ok(format!("{label} completed successfully via workflow"))
                }
                WorkflowStatus::Failed => {
                    let current_step = state.current_step.as_ref();
                    let step_name = current_step
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    let error_msg = current_step
                        .and_then(|step_id| state.step_states.get(step_id))
                        .and_then(|s| s.last_error.clone())
                        .unwrap_or_else(|| {
                            state
                                .error
                                .clone()
                                .unwrap_or_else(|| "Unknown error".into())
                        });
                    Err(format!("Workflow failed at step {step_name}: {error_msg}"))
                }
                WorkflowStatus::Cancelled => Err(format!("Workflow cancelled for {label}")),
                WorkflowStatus::Pending
                | WorkflowStatus::Paused
                | WorkflowStatus::Running
                | WorkflowStatus::Waiting => {
                    tokio::time::sleep(poll_interval).await;
                    poll_interval = (poll_interval + poll_backoff).min(max_poll_interval);
                    continue;
                }
            };

            // Completed/failed runs are retained until the TTL cleanup task
            // (`start_cleanup_task`) evicts them, so a status API can still
            // query them after completion.
            return result;
        }
    }

    fn clone_for_execution(&self) -> Self {
        Self {
            definitions: Arc::clone(&self.definitions),
            state_store: self.state_store.clone(),
            event_bus: Arc::clone(&self.event_bus),
            shutdown_tx: Arc::clone(&self.shutdown_tx),
            active_workflows: Arc::clone(&self.active_workflows),
            jitter: self.jitter,
            waiters: Arc::clone(&self.waiters),
            runtime: self.runtime.clone(),
            executor_id: Arc::clone(&self.executor_id),
            lease_ttl: self.lease_ttl,
            driving: Arc::clone(&self.driving),
            data_patches: Arc::clone(&self.data_patches),
        }
    }
}

impl<D: WorkflowData, S: StateStore<D> + 'static> Clone for WorkflowEngine<D, S> {
    fn clone(&self) -> Self {
        self.clone_for_execution()
    }
}

impl<D: WorkflowData> Default for WorkflowEngine<D, InMemoryStore<D>> {
    fn default() -> Self {
        Self::new()
    }
}

impl<D: WorkflowData, S: StateStore<D> + 'static> std::fmt::Debug for WorkflowEngine<D, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkflowEngine")
            .field("definitions_count", &self.definitions.read().len())
            .field(
                "active_workflows",
                &self.active_workflows.load(Ordering::Acquire),
            )
            .finish()
    }
}
