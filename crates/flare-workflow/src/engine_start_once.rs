//! Idempotent start-once, conditional orphan cancellation, and the generic
//! terminal-outcome callback for [`WorkflowEngine`](super::WorkflowEngine).
//! SQLite-specialized (durable receipts need SQL transactions); unrelated
//! `StateStore` implementations keep the existing non-idempotent start API.
//! Child module of `engine` (split out for size) so it can reach the
//! engine's private fields.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use super::{JournaledInput, StartGuard, WorkflowEngine};
use crate::events::WorkflowEvent;
use crate::sqlite_store::SqliteStore;
use crate::store::StateStore;
use crate::types::*;

/// Callback invoked once per terminal driver settlement (`Completed`,
/// `Failed`, or an actual `Cancelled` transition) with the run's final
/// status and error. Registered once per engine; `flare-workflow` knows
/// nothing about the caller's tables — the scheduler's callback writes its
/// own `schedule_runs` row. Must never fail the workflow: keep it to a short
/// fallible write that logs internally, since the tick fallback converges
/// anything it drops.
pub type TerminalCallback = Arc<
    dyn Fn(
            WorkflowRunId,
            WorkflowStatus,
            Option<String>,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>>
        + Send
        + Sync,
>;

/// Disposition of a conditional orphan-cancellation check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanCancelDisposition {
    /// Stale/missing lease on a live-state run: now `Cancelled`.
    Interrupted,
    /// Paused run or fresh lease: untouched.
    Unchanged,
    /// Already terminal: the real outcome (persisted by the caller).
    Terminal(WorkflowStatus),
    /// No such workflow row.
    Missing,
}

impl<D: WorkflowData, S: StateStore<D> + 'static> WorkflowEngine<D, S> {
    /// Register the terminal-outcome callback (at most one; replaces any
    /// previous). Fires on every terminal driver settlement from here on.
    pub fn on_terminal(&self, cb: TerminalCallback) {
        *self.terminal_callback.lock() = Some(cb);
    }

    /// Invoke the registered terminal callback, if any. Awaited on the
    /// driver's task right after the settle commits, so an eager write lands
    /// before any later eviction can remove the workflow row.
    pub(crate) async fn notify_terminal(
        &self,
        run_id: WorkflowRunId,
        status: WorkflowStatus,
        error: Option<String>,
    ) {
        let cb = self.terminal_callback.lock().clone();
        if let Some(cb) = cb {
            cb(run_id, status, error).await;
        }
    }
}

impl<D: WorkflowData> WorkflowEngine<D, SqliteStore<D>> {
    /// Idempotent start backed by a durable receipt: the receipt, the run's
    /// initial state, and its input journal entry commit in one SQLite
    /// transaction, and only the caller that created the receipt launches
    /// the driver. A repeated start for `start_key` returns the existing run
    /// ID — even after workflow-history eviction — without spawning,
    /// resuming, or rebuilding anything.
    #[must_use = "run ID should be stored or awaited"]
    pub async fn start_workflow_once_with_params(
        &self,
        start_key: &str,
        definition_id: WorkflowId,
        data: D,
        input: String,
        params: serde_json::Value,
    ) -> WorkflowResult<StartOnceResult> {
        // Fast path first: an existing receipt wins over everything,
        // including a missing workflow row or definition.
        if let Some(run_id) = self.state_store.lookup_start(start_key).await? {
            return Ok(StartOnceResult::Existing(run_id));
        }

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
        state.resume_identity = definition.run_identity(&state.context.data)?;
        state.budget = definition
            .budget
            .clone()
            .or_else(|| state.context.data.budget_limits())
            .map(|limits| crate::budget::BudgetState::new(limits, state.created_at))
            .transpose()?;
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
        let driving = self
            .claim_driving(run_id)
            .ok_or(WorkflowError::InvalidStateTransition {
                from: WorkflowStatus::Running,
                to: WorkflowStatus::Running,
            })?;
        let journaled_input = JournaledInput { input, params };
        let value = serde_json::to_vec(&journaled_input)
            .map_err(|e| WorkflowError::Journal(format!("serialize input: {e}")))?;

        match self
            .state_store
            .create_start_once(start_key.to_string(), state, JournalEntry::Input { value })
            .await?
        {
            // Lost the race (or the key predates us): someone else committed
            // first. Drop the candidate's guards; never launch.
            StartOnceResult::Existing(run_id) => Ok(StartOnceResult::Existing(run_id)),
            StartOnceResult::Created(run_id) => {
                // Post-commit maintenance must not turn this committed start
                // into an apparent failure: warn and carry on.
                if let Err(e) = self.evict_old_runs_if_needed().await {
                    tracing::warn!(run_id = %run_id, error = ?e, "start-once: post-commit eviction failed; run is still committed");
                }
                self.event_bus
                    .publish(WorkflowEvent::WorkflowStarted {
                        run_id,
                        definition_id,
                    })
                    .await;
                guard.commit();
                self.spawn_driven(
                    run_id,
                    Arc::clone(&definition),
                    self.active_workflow_guard(),
                    driving,
                );
                Ok(StartOnceResult::Created(run_id))
            }
        }
    }

    /// Conditionally cancel `run_id` iff it is an eligible orphan (live-state
    /// run whose executor lease is stale or missing at `now`): rechecks
    /// status and lease atomically under the store's mutation protection,
    /// persists the disposition consistently, and publishes a cancellation
    /// event only when state actually changed. The terminal callback is NOT
    /// invoked: the caller maps the returned disposition itself.
    /// Never use unconditional `cancel_workflow` here and assume
    /// interruption — a paused run or a fresh lease must survive.
    pub async fn cancel_if_orphaned(
        &self,
        run_id: WorkflowRunId,
        now: DateTime<Utc>,
    ) -> WorkflowResult<OrphanCancelDisposition> {
        use crate::sqlite_store::OrphanCancelOutcome;
        let outcome = self
            .state_store
            .cancel_orphaned_run(run_id, now, self.lease_ttl())
            .await?;
        Ok(match outcome {
            // No terminal-callback notification here: the caller initiated
            // this transition synchronously and maps the disposition itself
            // (the scheduler records `interrupted`, not `cancelled`). Firing
            // the eager hook as well would race that mapping and mislabel
            // the occurrence. Driver-driven settlements still notify.
            OrphanCancelOutcome::Cancelled => {
                self.event_bus
                    .publish(WorkflowEvent::WorkflowCancelled { run_id })
                    .await;
                OrphanCancelDisposition::Interrupted
            }
            OrphanCancelOutcome::Unchanged => OrphanCancelDisposition::Unchanged,
            OrphanCancelOutcome::Terminal(status) => OrphanCancelDisposition::Terminal(status),
            OrphanCancelOutcome::Missing => OrphanCancelDisposition::Missing,
        })
    }
}
