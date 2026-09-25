//! At most one driver per run: a start racing `recover()` is not driven twice,
//! a driver whose lease was taken over stops without settling the run, a
//! paused driver waits for its in-flight parallel siblings before letting go
//! of the run, and cancelling an already-settled run is a no-op.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use flare_workflow::engine::WorkflowEngine;
use flare_workflow::executor::FunctionStep;
use flare_workflow::types::*;
use flare_workflow::{InMemoryStore, StateStore, StepDefinition, WorkflowDefinition};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Ctx {
    log: Vec<String>,
}

impl WorkflowData for Ctx {
    fn workflow_type() -> &'static str {
        "driver-exclusivity-test"
    }
}

/// Counts its executions; blocks until `gate` is set (if given).
fn step(
    id: &'static str,
    runs: Arc<AtomicU32>,
    gate: Option<Arc<AtomicBool>>,
) -> StepDefinition<Ctx> {
    StepDefinition::new(
        id,
        id,
        Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
            runs.fetch_add(1, Ordering::SeqCst);
            ctx.data.log.push(id.to_string());
            let gate = gate.clone();
            Box::pin(async move {
                if let Some(gate) = gate {
                    while !gate.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                }
                Ok(StepResult::Success)
            })
        })),
    )
}

async fn wait_for<F: Fn() -> bool>(what: &str, f: F) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn wait_terminal<S: StateStore<Ctx> + 'static>(
    engine: &WorkflowEngine<Ctx, S>,
    run_id: WorkflowRunId,
) -> WorkflowState<Ctx> {
    for _ in 0..250 {
        let state = engine.get_status(run_id).await.unwrap();
        if matches!(
            state.status,
            WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled
        ) {
            return state;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("run {run_id} never reached a terminal state");
}

/// Delegates to `InMemoryStore`, but `save` lingers after persisting — widening
/// the window between a new run becoming visible to `list_active` and
/// `start_workflow` spawning its driver.
#[derive(Clone)]
struct SlowSaveStore {
    inner: InMemoryStore<Ctx>,
}

#[async_trait]
impl StateStore<Ctx> for SlowSaveStore {
    async fn save(&self, state: WorkflowState<Ctx>) -> WorkflowResult<()> {
        self.inner.save(state).await?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        Ok(())
    }
    async fn load(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowState<Ctx>> {
        self.inner.load(run_id).await
    }
    async fn update<F>(&self, run_id: WorkflowRunId, f: F) -> WorkflowResult<()>
    where
        F: FnOnce(&mut WorkflowState<Ctx>) + Send,
    {
        self.inner.update(run_id, f).await
    }
    async fn delete(&self, run_id: WorkflowRunId) -> WorkflowResult<()> {
        self.inner.delete(run_id).await
    }
    async fn list_active(&self) -> WorkflowResult<Vec<WorkflowState<Ctx>>> {
        self.inner.list_active().await
    }
    async fn list_all(&self) -> WorkflowResult<Vec<WorkflowState<Ctx>>> {
        self.inner.list_all().await
    }
    async fn is_cancelled(&self, run_id: WorkflowRunId) -> WorkflowResult<bool> {
        self.inner.is_cancelled(run_id).await
    }
    async fn cleanup_old_workflows(&self, ttl: Duration) -> usize {
        self.inner.cleanup_old_workflows(ttl).await
    }
    async fn get_context(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowContext<Ctx>> {
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
async fn recover_racing_start_workflow_does_not_drive_the_run_twice() {
    let runs = Arc::new(AtomicU32::new(0));
    let store = SlowSaveStore {
        inner: InMemoryStore::new(),
    };
    let engine = WorkflowEngine::with_store(store.clone());
    engine
        .register_workflow(WorkflowDefinition::new("wf", "wf").add_step(step(
            "a",
            runs.clone(),
            None,
        )))
        .unwrap();

    let starter = engine.clone();
    let start = tokio::spawn(async move {
        starter
            .start_workflow(WorkflowId::new("wf"), Ctx::default(), String::new())
            .await
    });
    // The run is persisted (Running, our own lease) but not yet spawned.
    for _ in 0..100 {
        if !store.list_active().await.unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(store.list_active().await.unwrap().len(), 1);
    assert!(
        engine.recover().await.unwrap().is_empty(),
        "recover must not claim a run start_workflow is about to drive"
    );

    let run_id = start.await.unwrap().unwrap();
    let state = wait_terminal(&engine, run_id).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    wait_for("driver to finish", || !engine.is_driving(run_id)).await;
    assert_eq!(runs.load(Ordering::SeqCst), 1, "step must run exactly once");
}

#[tokio::test]
async fn a_driver_whose_lease_was_taken_over_stops_without_settling_the_run() {
    let slow_runs = Arc::new(AtomicU32::new(0));
    let after_runs = Arc::new(AtomicU32::new(0));
    let gate = Arc::new(AtomicBool::new(false));
    let store = InMemoryStore::<Ctx>::new();
    let engine =
        WorkflowEngine::with_store(store.clone()).with_lease_ttl(Duration::from_millis(200));
    engine
        .register_workflow(
            WorkflowDefinition::new("wf", "wf")
                .add_step(step("slow", slow_runs.clone(), Some(gate.clone())))
                .add_step(step("after", after_runs.clone(), None).depends_on(&["slow"])),
        )
        .unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new("wf"), Ctx::default(), String::new())
        .await
        .unwrap();
    wait_for("slow step to start", || {
        slow_runs.load(Ordering::SeqCst) == 1
    })
    .await;

    // Another executor takes the run over (its lease stays fresh throughout).
    store
        .update(run_id, |s| {
            s.lease = Some(ExecutorLease {
                owner: "otherhost:2:y".into(),
                renewed_at: Utc::now() + chrono::Duration::seconds(60),
            });
        })
        .await
        .unwrap();
    // Let the renewer (every ttl/4 = 50ms) notice the takeover.
    tokio::time::sleep(Duration::from_millis(200)).await;
    gate.store(true, Ordering::SeqCst);
    wait_for("superseded driver to stop", || !engine.is_driving(run_id)).await;

    let state = engine.get_status(run_id).await.unwrap();
    assert_eq!(
        state.status,
        WorkflowStatus::Running,
        "the superseded driver must not settle the run"
    );
    assert_eq!(state.lease.unwrap().owner, "otherhost:2:y");
    assert_eq!(after_runs.load(Ordering::SeqCst), 0, "no further steps run");
    let journal = store.journal(run_id).await.unwrap();
    assert!(
        !journal.iter().any(|e| matches!(
            e,
            JournalEntry::StepRun { .. } | JournalEntry::Output { .. }
        )),
        "the superseded driver must journal nothing: {journal:?}"
    );
}

#[tokio::test]
async fn a_paused_driver_waits_for_running_parallel_siblings_before_letting_go() {
    let fast_runs = Arc::new(AtomicU32::new(0));
    let slow_runs = Arc::new(AtomicU32::new(0));
    let paused = Arc::new(AtomicBool::new(false));
    let release_slow = Arc::new(AtomicBool::new(false));
    let store = InMemoryStore::<Ctx>::new();
    let engine = WorkflowEngine::with_store(store.clone());
    engine
        .register_workflow(
            WorkflowDefinition::new("wf", "wf")
                .add_step(step("fast", fast_runs.clone(), Some(paused.clone())))
                .add_step(step("slow", slow_runs.clone(), Some(release_slow.clone()))),
        )
        .unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new("wf"), Ctx::default(), String::new())
        .await
        .unwrap();
    wait_for("both siblings to start", || {
        fast_runs.load(Ordering::SeqCst) == 1 && slow_runs.load(Ordering::SeqCst) == 1
    })
    .await;

    assert!(engine.pause_workflow(run_id).await.unwrap());
    // "fast" finishes; the driver sees the pause while "slow" still runs.
    paused.store(true, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(
        engine.is_driving(run_id),
        "the driver must hold the run while a sibling is still executing"
    );
    assert!(
        engine.resume_workflow(run_id).await.is_err(),
        "resume must be refused while a sibling is still executing"
    );

    release_slow.store(true, Ordering::SeqCst);
    wait_for("paused driver to stop", || !engine.is_driving(run_id)).await;
    assert_eq!(
        engine.get_status(run_id).await.unwrap().status,
        WorkflowStatus::Paused
    );

    assert!(engine.resume_workflow(run_id).await.unwrap());
    let state = wait_terminal(&engine, run_id).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(fast_runs.load(Ordering::SeqCst), 1);
    assert_eq!(
        slow_runs.load(Ordering::SeqCst),
        1,
        "the still-running sibling must not be relaunched"
    );
}

#[tokio::test]
async fn cancelling_an_already_settled_run_keeps_its_outcome() {
    let engine = WorkflowEngine::with_store(InMemoryStore::<Ctx>::new());
    engine
        .register_workflow(WorkflowDefinition::new("wf", "wf").add_step(step(
            "a",
            Arc::new(AtomicU32::new(0)),
            None,
        )))
        .unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new("wf"), Ctx::default(), String::new())
        .await
        .unwrap();
    assert_eq!(
        wait_terminal(&engine, run_id).await.status,
        WorkflowStatus::Completed
    );
    engine.cancel_workflow(run_id).await.unwrap();
    assert_eq!(
        engine.get_status(run_id).await.unwrap().status,
        WorkflowStatus::Completed
    );
}
