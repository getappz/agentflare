//! Out-of-band run control for [`WorkflowEngine`]: pause/resume and
//! cancellation (with optional saga rollback). Child module of `engine`
//! (split out for size) so it can reach the engine's private fields.

use super::WorkflowEngine;
use crate::events::WorkflowEvent;
use crate::store::StateStore;
use crate::types::*;

impl<D: WorkflowData, S: StateStore<D> + 'static> WorkflowEngine<D, S> {
    /// Pause a non-terminal run: the engine driving it stops at the next
    /// step boundary (a step already executing finishes or is stopped by its
    /// own cancellation, and is then re-run on resume rather than recorded
    /// as failed). The run's state, journal and data are kept as they are;
    /// paused runs are not picked up by `recover`. Returns whether the run
    /// was actually paused (false if it had already finished or was paused).
    pub async fn pause_workflow(&self, run_id: WorkflowRunId) -> WorkflowResult<bool> {
        let mut paused = false;
        self.state_store
            .update(run_id, |s| {
                if matches!(
                    s.status,
                    WorkflowStatus::Pending | WorkflowStatus::Running | WorkflowStatus::Waiting
                ) {
                    s.status = WorkflowStatus::Paused;
                    paused = true;
                }
            })
            .await?;
        if paused {
            tracing::info!(run_id = %run_id, "Workflow pause requested");
        }
        Ok(paused)
    }

    /// Resume a paused run on this engine, re-driving it from its persisted
    /// step (completed steps stay memoized via the journal). Returns false
    /// when the run isn't paused. Refused while the run is still being
    /// driven -- the paused driver hasn't reached its step boundary yet --
    /// since flipping it back to running then would let that driver carry
    /// on and count the stopped step as failed.
    pub async fn resume_workflow(&self, run_id: WorkflowRunId) -> WorkflowResult<bool> {
        let state = self.state_store.load(run_id).await?;
        if state.status != WorkflowStatus::Paused {
            return Ok(false);
        }
        if self.is_driving(run_id) || self.is_leased_elsewhere(&state) {
            return Err(WorkflowError::InvalidStateTransition {
                from: WorkflowStatus::Paused,
                to: WorkflowStatus::Running,
            });
        }
        self.resume_run(run_id).await
    }

    /// Put a step stopped by a pause back to `Pending` so its status doesn't
    /// read as failed/retrying while the run is paused.
    pub(crate) async fn mark_step_paused(
        &self,
        run_id: WorkflowRunId,
        step_id: &StepId,
    ) -> WorkflowResult<()> {
        self.state_store
            .update(run_id, |s| {
                if let Some(ss) = s.step_states.get_mut(step_id) {
                    ss.status = StepStatus::Pending;
                    ss.completed_at = None;
                }
            })
            .await
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

    pub(super) async fn cancel_workflow_impl(
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
}
