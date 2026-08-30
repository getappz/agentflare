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

/// Cheap end-to-end proof that `store`'s actual schema works: write a
/// throwaway state, delete it, and confirm a subsequent load reports it
/// gone. Exists because `SqliteStore::delete_state` silently used the wrong
/// column name on three of its four tables for ~33h before anyone noticed
/// (agentflare item #164/#576) — every claimed item just retried and failed
/// individually instead of surfacing anything actionable. Callers (daemon
/// boot) should refuse to dispatch entirely on failure rather than let that
/// repeat.
pub async fn smoke_test<D, S>(store: &S) -> WorkflowResult<()>
where
    D: WorkflowData + Default,
    S: StateStore<D>,
{
    let run_id = WorkflowRunId::new();
    let state = crate::types::WorkflowState::new(
        run_id,
        crate::types::WorkflowId::new("agentflare-store-smoke-test"),
        D::default(),
    );
    store.save(state).await?;
    store.delete(run_id).await?;
    match store.load(run_id).await {
        Err(WorkflowError::NotFound(_)) => Ok(()),
        Ok(_) => Err(WorkflowError::Store(
            "smoke test: state still loadable after delete()".into(),
        )),
        Err(e) => Err(e),
    }
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
            .filter(|s| {
                matches!(
                    s.status,
                    WorkflowStatus::Running | WorkflowStatus::Pending | WorkflowStatus::Waiting
                )
            })
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
                WorkflowStatus::Running
                    | WorkflowStatus::Pending
                    | WorkflowStatus::Waiting
                    | WorkflowStatus::Paused
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
    struct TestData;

    impl WorkflowData for TestData {
        fn workflow_type() -> &'static str {
            "smoke-test-data"
        }
    }

    #[tokio::test]
    async fn smoke_test_passes_against_a_working_store() {
        let store = InMemoryStore::<TestData>::new();
        smoke_test(&store).await.unwrap();
    }

    /// A store double whose `delete` is a no-op -- the exact shape of the
    /// bug `SqliteStore::delete_state` had (item #576): `save`/`load` work
    /// fine, but `delete` silently fails to remove the row, so a
    /// caller-observable `load()` after `delete()` still succeeds.
    #[derive(Clone, Default)]
    struct BrokenDeleteStore<D: WorkflowData> {
        inner: InMemoryStore<D>,
    }

    #[async_trait]
    impl<D: WorkflowData> StateStore<D> for BrokenDeleteStore<D> {
        async fn save(&self, state: WorkflowState<D>) -> WorkflowResult<()> {
            self.inner.save(state).await
        }
        async fn load(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowState<D>> {
            self.inner.load(run_id).await
        }
        async fn update<F>(&self, run_id: WorkflowRunId, f: F) -> WorkflowResult<()>
        where
            F: FnOnce(&mut WorkflowState<D>) + Send,
        {
            self.inner.update(run_id, f).await
        }
        async fn delete(&self, _run_id: WorkflowRunId) -> WorkflowResult<()> {
            Ok(())
        }
        async fn list_active(&self) -> WorkflowResult<Vec<WorkflowState<D>>> {
            self.inner.list_active().await
        }
        async fn list_all(&self) -> WorkflowResult<Vec<WorkflowState<D>>> {
            self.inner.list_all().await
        }
        async fn is_cancelled(&self, run_id: WorkflowRunId) -> WorkflowResult<bool> {
            self.inner.is_cancelled(run_id).await
        }
        async fn cleanup_old_workflows(&self, ttl: Duration) -> usize {
            self.inner.cleanup_old_workflows(ttl).await
        }
        async fn get_context(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowContext<D>> {
            self.inner.get_context(run_id).await
        }
        async fn cleanup_if_terminal(&self, run_id: WorkflowRunId) -> bool {
            self.inner.cleanup_if_terminal(run_id).await
        }
        async fn append_journal(
            &self,
            run_id: WorkflowRunId,
            entry: JournalEntry,
        ) -> WorkflowResult<u64> {
            self.inner.append_journal(run_id, entry).await
        }
        async fn journal(&self, run_id: WorkflowRunId) -> WorkflowResult<Vec<JournalEntry>> {
            self.inner.journal(run_id).await
        }
        async fn workflow_metrics(&self, filter: MetricsFilter) -> WorkflowResult<WorkflowMetrics> {
            self.inner.workflow_metrics(filter).await
        }
    }

    #[tokio::test]
    async fn smoke_test_fails_when_delete_does_not_remove_state() {
        let store = BrokenDeleteStore::<TestData>::default();
        let err = smoke_test(&store).await.unwrap_err();
        assert!(matches!(err, WorkflowError::Store(_)));
    }
}
