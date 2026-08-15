//! Workflow state persistence: the `StateStore` trait and the default
//! in-memory implementation.
//!
//! Ported from SMG `wfaas` state.rs (Apache-2.0), extended with journal
//! access so state and durability log live behind one interface.

use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use parking_lot::RwLock;

use crate::types::{
    JournalEntry, MetricsFilter, StepMetrics, WorkflowContext, WorkflowData, WorkflowError,
    WorkflowMetrics, WorkflowResult, WorkflowRunId, WorkflowState, WorkflowStatus,
};

/// Trait for workflow state persistence.
///
/// Implementations provide storage backends (in-memory, SQLite, ...). The
/// engine only ever talks to this trait.
#[async_trait]
pub trait StateStore<D: WorkflowData>: Send + Sync + Clone {
    /// Save workflow state.
    async fn save(&self, state: WorkflowState<D>) -> WorkflowResult<()>;

    /// Load workflow state by run ID.
    async fn load(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowState<D>>;

    /// Update workflow state using a closure.
    async fn update<F>(&self, run_id: WorkflowRunId, f: F) -> WorkflowResult<()>
    where
        F: FnOnce(&mut WorkflowState<D>) + Send;

    /// Delete workflow state.
    async fn delete(&self, run_id: WorkflowRunId) -> WorkflowResult<()>;

    /// List all active workflows (Running or Pending).
    async fn list_active(&self) -> WorkflowResult<Vec<WorkflowState<D>>>;

    /// List all workflows.
    async fn list_all(&self) -> WorkflowResult<Vec<WorkflowState<D>>>;

    /// Check if a workflow is cancelled without loading full state.
    async fn is_cancelled(&self, run_id: WorkflowRunId) -> WorkflowResult<bool>;

    /// Clean up old terminal workflows beyond a time threshold.
    async fn cleanup_old_workflows(&self, ttl: Duration) -> usize;

    /// Get just the workflow context without cloning the entire state.
    async fn get_context(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowContext<D>>;

    /// Clean up a specific workflow immediately if terminal.
    async fn cleanup_if_terminal(&self, run_id: WorkflowRunId) -> bool;

    /// Append an entry to a run's durable journal, returning its sequence.
    async fn append_journal(
        &self,
        run_id: WorkflowRunId,
        entry: JournalEntry,
    ) -> WorkflowResult<u64>;

    /// Read a run's durable journal in sequence order.
    async fn journal(&self, run_id: WorkflowRunId) -> WorkflowResult<Vec<JournalEntry>>;

    /// Aggregate instance/step metrics over runs matching `filter`.
    async fn workflow_metrics(&self, filter: MetricsFilter) -> WorkflowResult<WorkflowMetrics>;
}

/// In-memory state storage for workflow instances.
#[derive(Clone)]
pub struct InMemoryStore<D: WorkflowData> {
    states: Arc<RwLock<HashMap<WorkflowRunId, WorkflowState<D>>>>,
    journals: Arc<RwLock<HashMap<WorkflowRunId, Vec<JournalEntry>>>>,
}

impl<D: WorkflowData> InMemoryStore<D> {
    pub fn new() -> Self {
        Self {
            states: Arc::new(RwLock::new(HashMap::new())),
            journals: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get count of workflows by status.
    pub fn count_by_status(&self, status: WorkflowStatus) -> usize {
        self.states
            .read()
            .values()
            .filter(|s| s.status == status)
            .count()
    }

    /// Get total count of all workflows.
    pub fn count(&self) -> usize {
        self.states.read().len()
    }
}

impl<D: WorkflowData> Default for InMemoryStore<D> {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<D: WorkflowData> StateStore<D> for InMemoryStore<D> {
    async fn save(&self, state: WorkflowState<D>) -> WorkflowResult<()> {
        self.states.write().insert(state.run_id, state);
        Ok(())
    }

    async fn load(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowState<D>> {
        self.states
            .read()
            .get(&run_id)
            .cloned()
            .ok_or(WorkflowError::NotFound(run_id))
    }

    async fn update<F>(&self, run_id: WorkflowRunId, f: F) -> WorkflowResult<()>
    where
        F: FnOnce(&mut WorkflowState<D>) + Send,
    {
        let mut states = self.states.write();
        let state = states
            .get_mut(&run_id)
            .ok_or(WorkflowError::NotFound(run_id))?;
        f(state);
        state.updated_at = chrono::Utc::now();
        Ok(())
    }

    async fn delete(&self, run_id: WorkflowRunId) -> WorkflowResult<()> {
        self.states.write().remove(&run_id);
        self.journals.write().remove(&run_id);
        Ok(())
    }

    async fn list_active(&self) -> WorkflowResult<Vec<WorkflowState<D>>> {
        let states = self.states.read();
        Ok(states
            .values()
            .filter(|s| matches!(s.status, WorkflowStatus::Running | WorkflowStatus::Pending))
            .cloned()
            .collect())
    }

    async fn list_all(&self) -> WorkflowResult<Vec<WorkflowState<D>>> {
        let states = self.states.read();
        Ok(states.values().cloned().collect())
    }

    async fn is_cancelled(&self, run_id: WorkflowRunId) -> WorkflowResult<bool> {
        self.states
            .read()
            .get(&run_id)
            .map(|s| s.status == WorkflowStatus::Cancelled)
            .ok_or(WorkflowError::NotFound(run_id))
    }

    async fn cleanup_old_workflows(&self, ttl: Duration) -> usize {
        let now = chrono::Utc::now();
        let mut states = self.states.write();
        let initial_count = states.len();

        states.retain(|_, state| {
            if matches!(
                state.status,
                WorkflowStatus::Running | WorkflowStatus::Pending | WorkflowStatus::Paused
            ) {
                return true;
            }
            let age = now
                .signed_duration_since(state.updated_at)
                .to_std()
                .unwrap_or_default();
            age < ttl
        });

        let removed_count = initial_count - states.len();
        if removed_count > 0 {
            tracing::info!(
                removed = removed_count,
                remaining = states.len(),
                "Cleaned up old workflow states"
            );
        }
        removed_count
    }

    async fn get_context(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowContext<D>> {
        self.states
            .read()
            .get(&run_id)
            .map(|s| s.context.clone())
            .ok_or(WorkflowError::NotFound(run_id))
    }

    async fn cleanup_if_terminal(&self, run_id: WorkflowRunId) -> bool {
        let mut states = self.states.write();
        if let Some(state) = states.get(&run_id)
            && matches!(
                state.status,
                WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled
            )
        {
            states.remove(&run_id);
            self.journals.write().remove(&run_id);
            return true;
        }
        false
    }

    async fn append_journal(
        &self,
        run_id: WorkflowRunId,
        entry: JournalEntry,
    ) -> WorkflowResult<u64> {
        let mut journals = self.journals.write();
        let entries = journals.entry(run_id).or_default();
        entries.push(entry);
        Ok(entries.len() as u64)
    }

    async fn journal(&self, run_id: WorkflowRunId) -> WorkflowResult<Vec<JournalEntry>> {
        self.journals
            .read()
            .get(&run_id)
            .cloned()
            .ok_or(WorkflowError::NotFound(run_id))
    }

    async fn workflow_metrics(&self, filter: MetricsFilter) -> WorkflowResult<WorkflowMetrics> {
        let states = self.states.read();
        let matching: Vec<_> = states
            .values()
            .filter(|s| {
                filter
                    .workflow_id
                    .as_ref()
                    .is_none_or(|id| *id == s.workflow_id)
                    && filter.status.is_none_or(|status| status == s.status)
                    && filter.since.is_none_or(|since| s.created_at >= since)
            })
            .collect();

        let mut counts_by_status = HashMap::new();
        for state in &matching {
            *counts_by_status.entry(state.status).or_insert(0u64) += 1;
        }

        let durations: Vec<f64> = matching
            .iter()
            .map(|s| {
                s.updated_at
                    .signed_duration_since(s.created_at)
                    .num_milliseconds() as f64
            })
            .collect();
        let avg_duration_ms = if durations.is_empty() {
            None
        } else {
            Some(durations.iter().sum::<f64>() / durations.len() as f64)
        };

        let mut total_input_tokens = 0u64;
        let mut total_output_tokens = 0u64;
        let mut step_breakdown: HashMap<crate::types::StepId, StepMetrics> = HashMap::new();
        let mut step_durations: HashMap<crate::types::StepId, Vec<f64>> = HashMap::new();
        for state in &matching {
            for (step_id, ss) in &state.step_states {
                total_input_tokens += ss.input_tokens;
                total_output_tokens += ss.output_tokens;
                *step_breakdown
                    .entry(step_id.clone())
                    .or_default()
                    .counts_by_status
                    .entry(ss.status)
                    .or_insert(0u64) += 1;
                step_durations
                    .entry(step_id.clone())
                    .or_default()
                    .push(ss.duration_ms as f64);
            }
        }
        for (step_id, durations) in step_durations {
            let avg = durations.iter().sum::<f64>() / durations.len() as f64;
            step_breakdown.entry(step_id).or_default().avg_duration_ms = Some(avg);
        }

        Ok(WorkflowMetrics {
            counts_by_status,
            avg_duration_ms,
            total_input_tokens,
            total_output_tokens,
            step_breakdown,
        })
    }
}
