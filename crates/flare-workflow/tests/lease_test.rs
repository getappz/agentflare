//! Executor-lease liveness, per-run recovery isolation, and out-of-band data
//! patches (`patch_run_data`).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use flare_workflow::engine::WorkflowEngine;
use flare_workflow::executor::FunctionStep;
use flare_workflow::types::*;
use flare_workflow::{InMemoryStore, StateStore, StepDefinition, WorkflowDefinition};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Ctx {
    owner: String,
    log: Vec<String>,
}

impl WorkflowData for Ctx {
    fn workflow_type() -> &'static str {
        "lease-test"
    }
}

fn logging_step(id: &'static str) -> StepDefinition<Ctx> {
    StepDefinition::new(
        id,
        id,
        Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
            ctx.data.log.push(format!("{id}:{}", ctx.data.owner));
            Box::pin(async move { Ok(StepResult::Success) })
        })),
    )
}

fn one_step_wf() -> WorkflowDefinition<Ctx> {
    WorkflowDefinition::new("wf", "wf").add_step(logging_step("a"))
}

/// Persist a crashed-looking `Running` run of `one_step_wf` carrying `lease`.
async fn seed_run<S: StateStore<Ctx>>(store: &S, lease: Option<ExecutorLease>) -> WorkflowRunId {
    let run_id = WorkflowRunId::new();
    let mut state = WorkflowState::new(run_id, WorkflowId::new("wf"), Ctx::default());
    state.status = WorkflowStatus::Running;
    state
        .step_states
        .insert(StepId::new("a"), StepState::default());
    state.lease = lease;
    store.save(state).await.unwrap();
    store
        .append_journal(
            run_id,
            JournalEntry::Input {
                value: serde_json::to_vec(&serde_json::json!({"input": "", "params": null}))
                    .unwrap(),
            },
        )
        .await
        .unwrap();
    run_id
}

fn lease(owner: &str, age: Duration) -> ExecutorLease {
    ExecutorLease {
        owner: owner.to_string(),
        renewed_at: Utc::now() - chrono::Duration::from_std(age).unwrap(),
    }
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

#[tokio::test]
async fn recover_skips_a_run_leased_by_another_live_process() {
    let store = InMemoryStore::<Ctx>::new();
    let run_id = seed_run(&store, Some(lease("otherhost:42:abc", Duration::ZERO))).await;

    let engine = WorkflowEngine::with_store(store);
    engine.register_workflow(one_step_wf()).unwrap();
    let resumed = engine.recover().await.unwrap();

    assert!(
        resumed.is_empty(),
        "a fresh foreign lease must not be resumed"
    );
    tokio::time::sleep(Duration::from_millis(100)).await;
    let state = engine.get_status(run_id).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Running);
    assert!(state.context.data.log.is_empty(), "step must not have run");
    assert_eq!(state.lease.unwrap().owner, "otherhost:42:abc");
}

#[tokio::test]
async fn recover_takes_over_a_run_whose_lease_went_stale() {
    let store = InMemoryStore::<Ctx>::new();
    let stale = lease("deadhost:42:abc", Duration::from_secs(3600));
    let run_id = seed_run(&store, Some(stale)).await;

    let engine = WorkflowEngine::with_store(store);
    engine.register_workflow(one_step_wf()).unwrap();
    let resumed = engine.recover().await.unwrap();

    assert_eq!(resumed, vec![run_id]);
    let state = wait_terminal(&engine, run_id).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(state.context.data.log, vec!["a:".to_string()]);
}

#[tokio::test]
async fn recover_with_filter_leaves_vetoed_runs_untouched() {
    let store = InMemoryStore::<Ctx>::new();
    let vetoed = seed_run(&store, None).await;
    let kept = seed_run(&store, None).await;

    let engine = WorkflowEngine::with_store(store);
    engine.register_workflow(one_step_wf()).unwrap();
    let resumed = engine.recover_with(|s| s.run_id != vetoed).await.unwrap();

    assert_eq!(resumed, vec![kept]);
    wait_terminal(&engine, kept).await;
    let state = engine.get_status(vetoed).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Running);
    assert!(state.context.data.log.is_empty());
}

#[tokio::test]
async fn resume_run_respects_foreign_leases_and_does_not_double_drive() {
    let store = InMemoryStore::<Ctx>::new();
    let foreign = seed_run(&store, Some(lease("otherhost:1:x", Duration::ZERO))).await;
    let orphan = seed_run(
        &store,
        Some(lease("deadhost:1:x", Duration::from_secs(3600))),
    )
    .await;

    // Parks until released, so the orphan stays "being driven" while we
    // try to resume it a second time.
    let gate = Arc::new(tokio::sync::Notify::new());
    let step_gate = Arc::clone(&gate);
    let wf = WorkflowDefinition::new("wf", "wf").add_step(StepDefinition::new(
        "a",
        "a",
        Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
            ctx.data.log.push("a".into());
            let gate = Arc::clone(&step_gate);
            Box::pin(async move {
                gate.notified().await;
                Ok(StepResult::Success)
            })
        })),
    ));
    let engine = WorkflowEngine::with_store(store);
    engine.register_workflow(wf).unwrap();

    assert!(!engine.resume_run(foreign).await.unwrap());
    assert!(engine.resume_run(orphan).await.unwrap());
    assert!(engine.is_driving(orphan));
    assert!(
        !engine.resume_run(orphan).await.unwrap(),
        "a run this engine is already driving must not be resumed twice"
    );
    let lease = engine.get_status(orphan).await.unwrap().lease.unwrap();
    assert_eq!(lease.owner, engine.executor_id());

    gate.notify_one();
    let state = wait_terminal(&engine, orphan).await;
    assert_eq!(state.context.data.log, vec!["a".to_string()]);
    assert!(
        !engine.resume_run(orphan).await.unwrap(),
        "terminal runs are never resumed"
    );
}

#[tokio::test]
async fn lease_is_renewed_while_driving_and_released_when_done() {
    let wf = WorkflowDefinition::new("wf", "wf").add_step(StepDefinition::new(
        "slow",
        "slow",
        Arc::new(FunctionStep::new(|_ctx: &mut WorkflowContext<Ctx>| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(700)).await;
                Ok(StepResult::Success)
            })
        })),
    ));
    let engine = WorkflowEngine::with_store(InMemoryStore::<Ctx>::new())
        .with_lease_ttl(Duration::from_millis(200));
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new("wf"), Ctx::default(), String::new())
        .await
        .unwrap();

    let first = engine.get_status(run_id).await.unwrap().lease.unwrap();
    assert_eq!(first.owner, engine.executor_id());
    tokio::time::sleep(Duration::from_millis(400)).await;
    let later = engine.get_status(run_id).await.unwrap();
    let renewed = later.lease.clone().unwrap();
    assert!(
        renewed.renewed_at > first.renewed_at,
        "lease must be renewed"
    );
    assert!(
        renewed.is_fresh(Utc::now(), engine.lease_ttl()),
        "a driven run's lease must never go stale"
    );
    // Another process sharing the store must see it as held.
    let other = WorkflowEngine::with_store(engine.state_store().clone())
        .with_executor_id("otherhost:2:y")
        .with_lease_ttl(Duration::from_millis(200));
    assert!(other.is_leased_elsewhere(&later));

    let state = wait_terminal(&engine, run_id).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    // Release happens right after the terminal write.
    for _ in 0..50 {
        if engine.get_status(run_id).await.unwrap().lease.is_none() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(engine.get_status(run_id).await.unwrap().lease.is_none());
    assert!(!engine.is_driving(run_id));
}

#[tokio::test]
async fn patch_run_data_survives_an_in_flight_steps_write_back() {
    let gate = Arc::new(tokio::sync::Notify::new());
    let step_gate = Arc::clone(&gate);
    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(StepDefinition::new(
            "first",
            "first",
            Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
                ctx.data.log.push(format!("first:{}", ctx.data.owner));
                let gate = Arc::clone(&step_gate);
                Box::pin(async move {
                    gate.notified().await;
                    Ok(StepResult::Success)
                })
            })),
        ))
        .add_step(logging_step("second").depends_on(&["first"]));
    let engine = WorkflowEngine::with_store(InMemoryStore::<Ctx>::new());
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(
            WorkflowId::new("wf"),
            Ctx {
                owner: "agent:J1".into(),
                log: vec![],
            },
            String::new(),
        )
        .await
        .unwrap();

    // Wait until "first" is executing with the old owner, then rebind.
    for _ in 0..100 {
        let s = engine.get_status(run_id).await.unwrap();
        if s.step_states[&StepId::new("first")].status == StepStatus::Running {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    engine
        .patch_run_data(run_id, |d: &mut Ctx| d.owner = "agent:J2".into())
        .await
        .unwrap();
    gate.notify_one();

    let state = wait_terminal(&engine, run_id).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(
        state.context.data.owner, "agent:J2",
        "the in-flight step's stale write-back must not revert the patch"
    );
    assert_eq!(
        state.context.data.log,
        vec!["first:agent:J1".to_string(), "second:agent:J2".to_string()]
    );
}

/// Delegates to `InMemoryStore`, but `update` always fails for one run —
/// the shape of a single corrupt/unwritable row.
#[derive(Clone)]
struct FailingUpdateStore {
    inner: InMemoryStore<Ctx>,
    bad: Arc<parking_lot::Mutex<Option<WorkflowRunId>>>,
}

#[async_trait]
impl StateStore<Ctx> for FailingUpdateStore {
    async fn save(&self, state: WorkflowState<Ctx>) -> WorkflowResult<()> {
        self.inner.save(state).await
    }
    async fn load(&self, run_id: WorkflowRunId) -> WorkflowResult<WorkflowState<Ctx>> {
        self.inner.load(run_id).await
    }
    async fn update<F>(&self, run_id: WorkflowRunId, f: F) -> WorkflowResult<()>
    where
        F: FnOnce(&mut WorkflowState<Ctx>) + Send,
    {
        if *self.bad.lock() == Some(run_id) {
            return Err(WorkflowError::Store("simulated bad row".into()));
        }
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
async fn recover_isolates_a_failing_run_instead_of_aborting_the_sweep() {
    let store = FailingUpdateStore {
        inner: InMemoryStore::new(),
        bad: Arc::new(parking_lot::Mutex::new(None)),
    };
    let bad = seed_run(&store, None).await;
    let good = seed_run(&store, None).await;
    *store.bad.lock() = Some(bad);

    let engine = WorkflowEngine::with_store(store);
    engine.register_workflow(one_step_wf()).unwrap();
    let resumed = engine
        .recover()
        .await
        .expect("one bad row must not fail recover()");

    assert_eq!(resumed, vec![good]);
    assert!(!engine.is_driving(bad));
    let state = wait_terminal(&engine, good).await;
    assert_eq!(state.status, WorkflowStatus::Completed);
}

/// After a crash the dead predecessor's last renewal is usually still within
/// the TTL; a same-host holder whose pid is gone must not block recovery.
#[cfg(unix)]
#[tokio::test]
async fn recover_takes_over_a_fresh_lease_whose_same_host_holder_is_dead() {
    let store = InMemoryStore::<Ctx>::new();
    let engine = WorkflowEngine::with_store(store.clone());
    engine.register_workflow(one_step_wf()).unwrap();
    let host = engine
        .executor_id()
        .rsplitn(3, ':')
        .nth(2)
        .unwrap()
        .to_string();
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead_pid = child.id();
    child.wait().unwrap();

    let dead = seed_run(
        &store,
        Some(lease(&format!("{host}:{dead_pid}:gone"), Duration::ZERO)),
    )
    .await;
    let live = seed_run(
        &store,
        Some(lease(&format!("{host}:1:init"), Duration::ZERO)),
    )
    .await;

    let resumed = engine.recover().await.unwrap();
    assert_eq!(
        resumed,
        vec![dead],
        "only the dead holder's run is taken over"
    );
    wait_terminal(&engine, dead).await;
    assert_eq!(
        engine.get_status(live).await.unwrap().status,
        WorkflowStatus::Running
    );
}
