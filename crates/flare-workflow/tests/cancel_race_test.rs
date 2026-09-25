//! Cancellation vs. settlement: an operator cancel landing while a run is
//! settling keeps the run `Cancelled` (terminal settlement never overwrites
//! it, and publishes nothing), and `WorkflowCancelled` fires exactly once per
//! cancel whether or not a driver is live.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use flare_workflow::engine::WorkflowEngine;
use flare_workflow::events::{EventSubscriber, WorkflowEvent};
use flare_workflow::executor::FunctionStep;
use flare_workflow::types::*;
use flare_workflow::{InMemoryStore, StateStore, StepDefinition, WorkflowDefinition};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Ctx {
    log: Vec<String>,
}

impl WorkflowData for Ctx {
    fn workflow_type() -> &'static str {
        "cancel-race-test"
    }
}

type Engine = WorkflowEngine<Ctx, CancelOnJournalStore>;

/// Delegates to `InMemoryStore`; once the first journal entry matching
/// `trigger` is appended, cancels that run through `engine` -- i.e. an
/// operator cancel landing after the driver's last cancel check but before
/// it writes the run's terminal status.
#[derive(Clone)]
struct CancelOnJournalStore {
    inner: InMemoryStore<Ctx>,
    trigger: fn(&JournalEntry) -> bool,
    fired: Arc<AtomicBool>,
    engine: Arc<OnceLock<Engine>>,
}

#[async_trait]
impl StateStore<Ctx> for CancelOnJournalStore {
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
        let fire = (self.trigger)(&entry) && !self.fired.swap(true, Ordering::SeqCst);
        let seq = self.inner.append_journal(run_id, entry).await?;
        if fire {
            let engine = self.engine.get().expect("engine installed").clone();
            engine.cancel_workflow(run_id).await?;
        }
        Ok(seq)
    }
    async fn journal(&self, run_id: WorkflowRunId) -> WorkflowResult<Vec<JournalEntry>> {
        self.inner.journal(run_id).await
    }
    async fn workflow_metrics(&self, filter: MetricsFilter) -> WorkflowResult<WorkflowMetrics> {
        self.inner.workflow_metrics(filter).await
    }
}

/// Records the name of every run-level event it sees.
struct Recorder(Arc<Mutex<Vec<&'static str>>>);

#[async_trait]
impl EventSubscriber for Recorder {
    async fn on_event(&self, event: &WorkflowEvent) {
        let name = match event {
            WorkflowEvent::WorkflowCompleted { .. } => "completed",
            WorkflowEvent::WorkflowFailed { .. } => "failed",
            WorkflowEvent::WorkflowCancelled { .. } => "cancelled",
            _ => return,
        };
        self.0.lock().unwrap().push(name);
    }
}

fn build(trigger: fn(&JournalEntry) -> bool) -> (Engine, Arc<Mutex<Vec<&'static str>>>) {
    let store = CancelOnJournalStore {
        inner: InMemoryStore::new(),
        trigger,
        fired: Arc::new(AtomicBool::new(false)),
        engine: Arc::new(OnceLock::new()),
    };
    let engine = WorkflowEngine::with_store(store.clone());
    let _ = store.engine.set(engine.clone());
    let events = Arc::new(Mutex::new(Vec::new()));
    (engine, events)
}

async fn subscribe(engine: &Engine, events: &Arc<Mutex<Vec<&'static str>>>) {
    engine
        .event_bus()
        .subscribe(Arc::new(Recorder(Arc::clone(events))))
        .await;
}

fn ok_step(id: &'static str) -> StepDefinition<Ctx> {
    StepDefinition::new(
        id,
        id,
        Arc::new(FunctionStep::new(|_ctx: &mut WorkflowContext<Ctx>| {
            Box::pin(async move { Ok(StepResult::Success) })
        })),
    )
}

/// Succeeds once `gate` opens.
fn gated_step(id: &'static str, gate: Arc<AtomicBool>) -> StepDefinition<Ctx> {
    StepDefinition::new(
        id,
        id,
        Arc::new(FunctionStep::new(move |_ctx: &mut WorkflowContext<Ctx>| {
            let gate = Arc::clone(&gate);
            Box::pin(async move {
                while !gate.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Ok(StepResult::Success)
            })
        })),
    )
}

fn failing_step(id: &'static str) -> StepDefinition<Ctx> {
    StepDefinition::new(
        id,
        id,
        Arc::new(FunctionStep::new(|_ctx: &mut WorkflowContext<Ctx>| {
            Box::pin(async move {
                Err(WorkflowError::StepFailed {
                    step_id: StepId::new("boom"),
                    message: "boom".into(),
                })
            })
        })),
    )
    .with_retry(RetryPolicy {
        max_attempts: 1,
        backoff: BackoffStrategy::Fixed(Duration::from_millis(1)),
    })
}

/// Waits until nothing drives `run_id` and fire-and-forget event delivery
/// has had time to land.
async fn settle(engine: &Engine, run_id: WorkflowRunId) {
    for _ in 0..500 {
        if !engine.is_driving(run_id) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(!engine.is_driving(run_id), "driver never exited");
    tokio::time::sleep(Duration::from_millis(150)).await;
}

#[tokio::test]
async fn cancel_racing_completion_keeps_cancelled_and_publishes_no_completed() {
    let (engine, events) = build(|e| matches!(e, JournalEntry::Output { .. }));
    subscribe(&engine, &events).await;
    engine
        .register_workflow(WorkflowDefinition::new("wf", "wf").add_step(ok_step("a")))
        .unwrap();
    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx::default(), String::new())
        .await
        .unwrap();
    settle(&engine, run).await;

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Cancelled);
    assert_eq!(*events.lock().unwrap(), vec!["cancelled"]);
}

#[tokio::test]
async fn cancel_racing_failure_keeps_cancelled_and_publishes_no_failed() {
    // Fires on the rollback phase's first journal entry, which runs right
    // before the run would be settled as `Failed`.
    let (engine, events) = build(|e| matches!(e, JournalEntry::Rollback { .. }));
    subscribe(&engine, &events).await;
    engine
        .register_workflow(
            WorkflowDefinition::new("wf", "wf")
                .add_step(ok_step("a").with_rollback(
                    Arc::new(FunctionStep::new(|_ctx: &mut WorkflowContext<Ctx>| {
                        Box::pin(async move { Ok(StepResult::Success) })
                    })),
                    None,
                ))
                .add_step(failing_step("b").depends_on(&["a"])),
        )
        .unwrap();
    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx::default(), String::new())
        .await
        .unwrap();
    settle(&engine, run).await;

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Cancelled);
    assert_eq!(*events.lock().unwrap(), vec!["cancelled"]);
}

// A live driver observing the cancel must not announce it a second time.
#[tokio::test]
async fn cancel_of_a_driven_run_publishes_cancelled_once() {
    let (engine, events) = build(|_| false);
    subscribe(&engine, &events).await;
    let gate = Arc::new(AtomicBool::new(false));
    engine
        .register_workflow(
            WorkflowDefinition::new("wf", "wf")
                .add_step(gated_step("a", Arc::clone(&gate)))
                .add_step(ok_step("b").depends_on(&["a"])),
        )
        .unwrap();
    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx::default(), String::new())
        .await
        .unwrap();
    assert!(engine.is_driving(run));
    engine.cancel_workflow(run).await.unwrap();
    gate.store(true, Ordering::SeqCst);
    settle(&engine, run).await;

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Cancelled);
    assert_eq!(*events.lock().unwrap(), vec!["cancelled"]);
}

// With no driver to observe it, the cancel itself must still announce it.
#[tokio::test]
async fn cancel_of_an_undriven_run_publishes_cancelled_once() {
    let (engine, events) = build(|_| false);
    subscribe(&engine, &events).await;
    let gate = Arc::new(AtomicBool::new(false));
    engine
        .register_workflow(
            WorkflowDefinition::new("wf", "wf")
                .add_step(gated_step("a", Arc::clone(&gate)))
                .add_step(ok_step("b").depends_on(&["a"])),
        )
        .unwrap();
    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx::default(), String::new())
        .await
        .unwrap();
    assert!(engine.pause_workflow(run).await.unwrap());
    gate.store(true, Ordering::SeqCst);
    settle(&engine, run).await;
    engine.cancel_workflow(run).await.unwrap();
    // A second cancel of the settled run is a no-op.
    engine.cancel_workflow(run).await.unwrap();
    tokio::time::sleep(Duration::from_millis(150)).await;

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Cancelled);
    assert_eq!(*events.lock().unwrap(), vec!["cancelled"]);
}
