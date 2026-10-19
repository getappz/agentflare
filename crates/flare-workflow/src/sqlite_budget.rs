use super::*;
use crate::budget::{BudgetOperation, BudgetState, BudgetUpdate};

/// Read-only projection, including databases created before the budget migration.
pub fn read_budget(
    conn: &Connection,
    run_id: WorkflowRunId,
    seed: Option<BudgetState>,
) -> WorkflowResult<Option<BudgetState>> {
    let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='workflow_budgets')", [], |row| row.get(0))
        .map_err(|e| WorkflowError::Store(format!("budget table: {e}")))?;
    if !exists {
        return Ok(seed);
    }
    let json: Option<String> = conn
        .query_row(
            "SELECT budget_json FROM workflow_budgets WHERE run_id=?1",
            [run_id.to_string()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| WorkflowError::Store(format!("read budget: {e}")))?;
    json.map(|json| {
        serde_json::from_str(&json)
            .map_err(|e| WorkflowError::Store(format!("deserialize budget: {e}")))
    })
    .transpose()
    .map(|budget| budget.or(seed))
}

impl<D: WorkflowData> SqliteStore<D> {
    pub(super) async fn mutate_budget(
        &self,
        run_id: WorkflowRunId,
        operation: BudgetOperation,
    ) -> WorkflowResult<BudgetUpdate> {
        let conn = Arc::clone(&self.conn);
        blocking(move || {
            let mut conn = conn.lock().map_err(|e| WorkflowError::Store(format!("lock: {e}")))?;
            // Acquire the write lock before reading balances, across processes as well as clones.
            let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| WorkflowError::Store(format!("begin budget reservation: {e}")))?;
            let json: String = tx.query_row("SELECT state_json FROM workflow_runs WHERE id=?1", [run_id.to_string()], |row| row.get(0))
                .optional().map_err(|e| WorkflowError::Store(format!("load budget run: {e}")))?.ok_or(WorkflowError::NotFound(run_id))?;
            let state = Self::deserialize(&json)?;
            let snapshot = matches!(&operation, BudgetOperation::Snapshot);
            if matches!(&operation, BudgetOperation::Reserve { .. }) && (state.status.is_terminal() || state.status == WorkflowStatus::Paused) {
                return Err(crate::budget::error("run is no longer dispatchable"));
            }
            let seed = state.budget.ok_or_else(|| crate::budget::error("run has no budget"))?;
            let mut budget = read_budget(&tx, run_id, Some(seed.clone()))?.ok_or_else(||crate::budget::error("run has no budget"))?;
            if budget.limits != seed.limits || budget.started_at != seed.started_at {
                return Err(crate::budget::error("persisted budget policy changed; start a new run"));
            }
            let update = budget.apply(operation)?;
            if !snapshot {
                let json = serde_json::to_string(&budget).map_err(|e| WorkflowError::Store(format!("serialize budget: {e}")))?;
                tx.execute("INSERT INTO workflow_budgets(run_id,budget_json) VALUES(?1,?2) ON CONFLICT(run_id) DO UPDATE SET budget_json=excluded.budget_json", params![run_id.to_string(),json])
                    .map_err(|e| WorkflowError::Store(format!("persist budget: {e}")))?;
            }
            tx.commit().map_err(|e| WorkflowError::Store(format!("commit budget: {e}")))?;
            Ok(update)
        }).await
    }
}
