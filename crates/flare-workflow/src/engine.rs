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
        atomic::{AtomicUsize, Ordering},
        Arc,
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
        Self { engine, committed: false }
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
            || depends_on_any.iter().any(|dep| {
                self.completed.contains(dep) || self.skipped.contains(dep)
            })
    }

    fn has_failed_dependency(&self, depends_on: &[StepId]) -> bool {
        depends_on.iter().any(|dep| self.failed.contains(dep))
    }

    fn have_all_any_deps_failed(&self, depends_on_any: &[StepId]) -> bool {
        !depends_on_any.is_empty()
            && depends_on_any.iter().all(|dep| self.failed.contains(dep))
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
    state_store: S,
    event_bus: Arc<EventBus>,
    shutdown_tx: Arc<watch::Sender<bool>>,
    active_workflows: Arc<AtomicUsize>,
    /// Jitter factor applied to retry backoff delays (0.0-1.0).
    jitter: f64,
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
        }
    }

    /// Set the jitter factor applied to retry backoff delays.
    pub fn with_jitter(mut self, jitter: f64) -> Self {
        self.jitter = jitter.clamp(0.0, 1.0);
        self
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

        tokio::spawn(async move {
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
        state.input = input.clone();

        state.step_states.reserve(definition.steps.len());
        for step in &definition.steps {
            state.step_states.insert(step.id.clone(), StepState::default());
        }

        self.state_store.save(state).await?;
        self.state_store
            .append_journal(run_id, JournalEntry::Input { value: input.into_bytes() })
            .await?;

        self.event_bus
            .publish(WorkflowEvent::WorkflowStarted { run_id, definition_id })
            .await;

        guard.commit();

        let engine = self.clone_for_execution();
        let def = Arc::clone(&definition);
        tokio::spawn(async move {
            let _guard = engine.active_workflow_guard();
            if let Err(e) = engine.execute_workflow(run_id, def).await {
                tracing::error!(run_id = %run_id, error = ?e, "Workflow execution failed");
            }
        });

        Ok(run_id)
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
        let (tx, mut rx) = mpsc::channel::<(StepId, StepResult)>(step_count.max(1));

        let mut pending_check: VecDeque<usize> = definition
            .get_initial_step_indices()
            .iter()
            .copied()
            .collect();

        loop {
            if self.state_store.is_cancelled(run_id).await? {
                self.event_bus
                    .publish(WorkflowEvent::WorkflowCancelled { run_id })
                    .await;
                return Ok(());
            }

            // Phase 0: drain completion signals so dependents are added.
            while let Ok((step_id, result)) = rx.try_recv() {
                if matches!(result, StepResult::Success | StepResult::Skip) {
                    for &dep_idx in definition.get_dependent_indices(&step_id) {
                        pending_check.push_back(dep_idx);
                    }
                }
            }

            // Phase 1: check waiting steps + deps-ready steps + blocked detection.
            let (newly_ready_from_wait, deps_ready_indices, total_processed, current_running, current_waiting) =
            {
                let t = tracker.read();
                let wait_ready = t.get_ready_waiting_indices();
                let deps_ready: Vec<usize> = pending_check
                    .drain(..)
                    .filter(|&idx| {
                        let step = &definition.steps[idx];
                        t.is_step_processable(&step.id, idx)
                            && t.are_dependencies_satisfied(&step.depends_on)
                            && t.is_any_dependency_satisfied(&step.depends_on_any)
                            && !t.has_failed_dependency(&step.depends_on)
                            && !t.have_all_any_deps_failed(&step.depends_on_any)
                    })
                    .collect();
                (wait_ready, deps_ready, t.total_processed(), t.running.len(), t.waiting_until.len())
            };

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
                while let Ok((step_id, result)) = rx.try_recv() {
                    drained_completion = true;
                    if matches!(result, StepResult::Success | StepResult::Skip) {
                        for &dep_idx in definition.get_dependent_indices(&step_id) {
                            pending_check.push_back(dep_idx);
                        }
                    }
                }
                if drained_completion {
                    continue;
                }

                let failed_step = tracker.read().failed.iter().next().cloned();
                let error_message: &'static str = if failed_step.is_some() {
                    "Workflow failed due to step dependency failure"
                } else {
                    "Workflow deadlocked: no steps ready and none running"
                };
                self.state_store
                    .update(run_id, |s| {
                        s.status = WorkflowStatus::Failed;
                        s.error = Some(error_message.to_string());
                    })
                    .await?;
                self.event_bus
                    .publish(WorkflowEvent::WorkflowFailed {
                        run_id,
                        failed_step: failed_step.unwrap_or_else(|| StepId::new("internal_scheduler")),
                        error: error_message.to_string(),
                    })
                    .await;
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
                                            ss.last_error = Some(format!("run_if context error: {e}"));
                                        }
                                    })
                                    .await;
                                return;
                            }
                        }
                    }

                    let result = engine.execute_step_with_retry(run_id, step, &def).await;

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
                            Ok(StepResult::Failure) | Err(_) => match step.on_failure {
                                FailureAction::FailWorkflow | FailureAction::RetryIndefinitely => {
                                    t.failed.insert(step_id.clone());
                                    (StepResult::Failure, false)
                                }
                                FailureAction::ContinueNextStep => {
                                    t.skipped.insert(step_id.clone());
                                    (StepResult::Skip, true)
                                }
                            },
                        };

                        if let Err(e) = tx.try_send((step_id.clone(), sig)) {
                            use mpsc::error::TrySendError;
                            match e {
                                TrySendError::Full(_) => tracing::error!(step_id = %step_id, "Channel full sending step completion"),
                                TrySendError::Closed(_) => tracing::debug!(step_id = %step_id, "Channel closed, workflow likely cancelled"),
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
                (
                    !t.running.is_empty(),
                    !t.waiting_until.is_empty(),
                )
            };

            if has_running {
                if let Some((completed_step_id, result)) = rx.recv().await
                    && matches!(result, StepResult::Success | StepResult::Skip)
                {
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

        if let Some(ref step) = failed_step {
            self.state_store
                .update(run_id, |s| {
                    s.status = WorkflowStatus::Failed;
                    s.error = Some("One or more steps failed".to_string());
                })
                .await?;
            self.event_bus
                .publish(WorkflowEvent::WorkflowFailed {
                    run_id,
                    failed_step: step.clone(),
                    error: "One or more steps failed".into(),
                })
                .await;
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
                    JournalEntry::Output { result: EntryResult::Success(output.into_bytes()) },
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
                        ss.status = if attempt == 1 { StepStatus::Running } else { StepStatus::Retrying };
                        ss.attempt = attempt;
                        ss.started_at = Some(Utc::now());
                    }
                })
                .await?;

            self.event_bus
                .publish(WorkflowEvent::StepStarted { run_id, step_id: step.id.clone(), attempt })
                .await;

            let mut context = self.state_store.get_context(run_id).await?;
            let step_start = std::time::Instant::now();
            let result = timeout(step_timeout, step.executor.execute(&mut context)).await;
            let step_duration = step_start.elapsed();

            if !matches!(result, Ok(Ok(StepResult::Skip))) {
                self.state_store
                    .update(run_id, |s| {
                        s.context = context.clone();
                    })
                    .await?;
            }

            let context_bytes = serde_json::to_vec(&context)
                .map_err(|e| WorkflowError::Journal(format!("serialize context: {e}")))?;

            match result {
                Ok(Ok(StepResult::Success)) => {
                    self.state_store
                        .update(run_id, |s| {
                            if let Some(ss) = s.step_states.get_mut(&step.id) {
                                ss.status = StepStatus::Succeeded;
                                ss.completed_at = Some(Utc::now());
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
                Ok(Ok(StepResult::Failure)) | Ok(Err(_)) | Err(_) => {
                    let (error_msg, should_retry) = match result {
                        Ok(Err(e)) => {
                            let retryable = step.executor.is_retryable(&e);
                            (format!("{e}"), retryable)
                        }
                        Err(_) => (format!("Step timeout after {step_timeout:?}"), true),
                        _ => ("Step failed".to_string(), false),
                    };

                    let will_retry = should_retry && attempt < max_attempts;

                    self.state_store
                        .update(run_id, |s| {
                            if let Some(ss) = s.step_states.get_mut(&step.id) {
                                ss.status = if will_retry { StepStatus::Retrying } else { StepStatus::Failed };
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
                        let delay = backoff.next(self.jitter).unwrap_or_else(|| Duration::from_secs(1));
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
                        return Ok(StepResult::Failure);
                    }
                }
            }
        }
    }

    /// Cancel a running workflow.
    pub async fn cancel_workflow(&self, run_id: WorkflowRunId) -> WorkflowResult<()> {
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
                return Err(format!("Workflow timeout after {}s for {label}", timeout_duration.as_secs()));
            }

            let state = self.get_status(run_id).await.map_err(|e| format!("Failed to get status: {e:?}"))?;

            let result = match state.status {
                WorkflowStatus::Completed => Ok(format!("{label} completed successfully via workflow")),
                WorkflowStatus::Failed => {
                    let current_step = state.current_step.as_ref();
                    let step_name = current_step.map(|s| s.to_string()).unwrap_or_else(|| "unknown".to_string());
                    let error_msg = current_step
                        .and_then(|step_id| state.step_states.get(step_id))
                        .and_then(|s| s.last_error.clone())
                        .unwrap_or_else(|| state.error.clone().unwrap_or_else(|| "Unknown error".into()));
                    Err(format!("Workflow failed at step {step_name}: {error_msg}"))
                }
                WorkflowStatus::Cancelled => Err(format!("Workflow cancelled for {label}")),
                WorkflowStatus::Pending | WorkflowStatus::Paused | WorkflowStatus::Running => {
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
            .field("active_workflows", &self.active_workflows.load(Ordering::Acquire))
            .finish()
    }
}
