use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use flare_workflow::engine::WorkflowEngine;
use flare_workflow::executor::FunctionStep;
use flare_workflow::store::InMemoryStore;
use flare_workflow::types::*;
use flare_workflow::{StateStore, StepDefinition, WorkflowDefinition};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Ctx {
    count: i32,
    log: Vec<String>,
}

impl WorkflowData for Ctx {
    fn workflow_type() -> &'static str {
        "test"
    }
}

fn step(id: &str, f: impl Fn(&mut WorkflowContext<Ctx>) + Send + Sync + 'static) -> StepDefinition<Ctx> {
    StepDefinition::new(id, id, Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
        f(ctx);
        Box::pin(async move { Ok(StepResult::Success) })
    })))
}

fn build_engine() -> WorkflowEngine<Ctx, InMemoryStore<Ctx>> {
    WorkflowEngine::new()
}

#[tokio::test]
async fn sequential_chain_executes_in_order() {
    let engine = build_engine();
    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(step("a", |c| {
            c.data.count += 1;
            c.data.log.push("a".into());
        }))
        .add_step(step("b", |c| {
            c.data.count += 1;
            c.data.log.push("b".into());
        }))
        .add_step(step("c", |c| {
            c.data.count += 1;
            c.data.log.push("c".into());
        }))
        .add_step(step("d", |c| {
            c.data.count += 1;
            c.data.log.push("d".into());
        }));
    engine.register_workflow(wf).unwrap();

    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { count: 0, log: vec![] }, "in".into())
        .await
        .unwrap();
    let out = engine.wait_for_completion(run, "wf", Duration::from_secs(5)).await.unwrap();

    assert!(out.contains("completed successfully"));
    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(state.context.data.count, 4);
    assert_eq!(state.context.data.log, vec!["a", "b", "c", "d"]);
}

#[tokio::test]
async fn dag_parallel_join_orders_dependents() {
    let engine = build_engine();
    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(step("a", |c| {
            std::thread::sleep(Duration::from_millis(20));
            c.data.log.push("a".into());
        }))
        .add_step(step("b", |c| c.data.log.push("b".into())))
        .add_step(step("join", |c| c.data.log.push("join".into())).depends_on(&["a", "b"]));
    engine.register_workflow(wf).unwrap();

    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { count: 0, log: vec![] }, "in".into())
        .await
        .unwrap();
    engine.wait_for_completion(run, "wf", Duration::from_secs(5)).await.unwrap();

    let state = engine.get_status(run).await.unwrap();
    let log = state.context.data.log;
    // a and b ran before join; both completed.
    assert!(log.contains(&"a".to_string()));
    assert!(log.contains(&"b".to_string()));
    assert_eq!(log.last(), Some(&"join".to_string()));
}

#[tokio::test]
async fn retries_until_success_with_backoff() {
    let engine = build_engine();
    let calls = Arc::new(AtomicU32::new(0));
    let cc = Arc::clone(&calls);

    let wf = WorkflowDefinition::new("wf", "wf").add_step(
        StepDefinition::new(
            "flaky",
            "flaky",
            Arc::new(FunctionStep::new(move |_ctx: &mut WorkflowContext<Ctx>| {
                let n = cc.fetch_add(1, Ordering::SeqCst);
                let fail = n < 2;
                Box::pin(async move {
                    if fail {
                        Err(WorkflowError::StepFailed {
                            step_id: StepId::new("flaky"),
                            message: "transient".into(),
                        })
                    } else {
                        Ok(StepResult::Success)
                    }
                })
            })))
            .with_retry(RetryPolicy {
                max_attempts: 3,
                backoff: BackoffStrategy::Fixed(Duration::from_millis(1)),
            }),
    );
    engine.register_workflow(wf).unwrap();

    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { count: 0, log: vec![] }, "in".into())
        .await
        .unwrap();
    engine.wait_for_completion(run, "wf", Duration::from_secs(5)).await.unwrap();

    // 2 failures + 1 success = 3 attempts; step succeeds after retry.
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let state = engine.get_status(run).await.unwrap();
    let ss = &state.step_states[&StepId::new("flaky")];
    assert_eq!(ss.status, StepStatus::Succeeded);
    assert_eq!(ss.attempt, 3);
}

#[tokio::test]
async fn terminal_failure_marks_run_failed() {
    let engine = build_engine();
    let wf = WorkflowDefinition::new("wf", "wf").add_step(
        StepDefinition::new(
            "boom",
            "boom",
            Arc::new(FunctionStep::new(|_ctx: &mut WorkflowContext<Ctx>| {
                Box::pin(async move {
                    Err(WorkflowError::StepFailed {
                        step_id: StepId::new("boom"),
                        message: "boom".into(),
                    })
                })
            }))),
    );
    engine.register_workflow(wf).unwrap();

    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { count: 0, log: vec![] }, "in".into())
        .await
        .unwrap();
    let out = engine
        .wait_for_completion(run, "wf", Duration::from_secs(5))
        .await;
    assert!(out.is_err());
    assert!(out.unwrap_err().contains("boom"));

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Failed);
}

#[tokio::test]
async fn journal_records_every_step_result() {
    let engine = build_engine();
    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(step("a", |c| c.data.count += 1))
        .add_step(step("b", |c| c.data.count += 1));
    engine.register_workflow(wf).unwrap();

    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { count: 0, log: vec![] }, "in".into())
        .await
        .unwrap();
    engine.wait_for_completion(run, "wf", Duration::from_secs(5)).await.unwrap();

    let journal = engine.state_store().journal(run).await.unwrap();
    let step_runs: Vec<_> = journal
        .iter()
        .filter(|e| matches!(e, JournalEntry::StepRun { .. }))
        .collect();
    assert_eq!(step_runs.len(), 2, "journal should record each completed step");
    for e in step_runs {
        assert!(e.is_completed(), "completed step entries must carry a result");
    }
    assert!(journal.iter().any(|e| matches!(e, JournalEntry::Input { .. })));
    assert!(journal.iter().any(|e| matches!(e, JournalEntry::Output { .. })));
}

#[tokio::test]
async fn cancellation_stops_execution() {
    let engine = build_engine();
    let wf = WorkflowDefinition::new("wf", "wf").add_step(
        step("slow", |_| std::thread::sleep(Duration::from_millis(3000))),
    );
    engine.register_workflow(wf).unwrap();

    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { count: 0, log: vec![] }, "in".into())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    engine.cancel_workflow(run).await.unwrap();

    // After cancellation the run never completes; status is cancelled.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Cancelled);
}

#[tokio::test]
async fn run_if_skip_counts_as_satisfied() {
    let engine = build_engine();
    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(
            step("maybe", |c| c.data.log.push("maybe".into()))
                .run_if(|ctx| ctx.data.count > 10),
        )
        .add_step(step("after", |c| c.data.log.push("after".into())).depends_on(&["maybe"]));
    engine.register_workflow(wf).unwrap();

    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { count: 0, log: vec![] }, "in".into())
        .await
        .unwrap();
    engine.wait_for_completion(run, "wf", Duration::from_secs(5)).await.unwrap();

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.context.data.log, vec!["after".to_string()]);
    assert_eq!(
        state.step_states[&StepId::new("maybe")].status,
        StepStatus::Skipped
    );
}

#[tokio::test]
async fn event_bus_delivers_lifecycle_events() {
    use flare_workflow::events::{EventBus, EventSubscriber, WorkflowEvent};
    use std::sync::Mutex;

    let engine = build_engine();
    let bus: Arc<EventBus> = engine.event_bus();
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    struct Recorder(Arc<Mutex<Vec<String>>>);
    #[async_trait::async_trait]
    impl EventSubscriber for Recorder {
        async fn on_event(&self, event: &WorkflowEvent) {
            let name = match event {
                WorkflowEvent::WorkflowStarted { .. } => "started",
                WorkflowEvent::StepStarted { .. } => "step_started",
                WorkflowEvent::StepSucceeded { .. } => "step_succeeded",
                WorkflowEvent::WorkflowCompleted { .. } => "completed",
                _ => "other",
            };
            self.0.lock().unwrap().push(name.to_string());
        }
    }
    bus.subscribe(Arc::new(Recorder(Arc::clone(&events)))).await;

    let wf = WorkflowDefinition::new("wf", "wf").add_step(step("a", |c| c.data.count += 1));
    engine.register_workflow(wf).unwrap();
    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { count: 0, log: vec![] }, "in".into())
        .await
        .unwrap();
    engine.wait_for_completion(run, "wf", Duration::from_secs(5)).await.unwrap();
    // Wait for fire-and-forget delivery.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let got = events.lock().unwrap().clone();
    assert!(got.contains(&"started".to_string()));
    assert!(got.contains(&"step_started".to_string()));
    assert!(got.contains(&"step_succeeded".to_string()));
    assert!(got.contains(&"completed".to_string()));
}
