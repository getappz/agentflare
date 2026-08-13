use std::sync::Arc;
use std::time::Duration;

use flare_workflow::engine::WorkflowEngine;
use flare_workflow::executor::{FunctionStep, noop_executor};
use flare_workflow::store::InMemoryStore;
use flare_workflow::types::*;
use flare_workflow::{EntryResult, StateStore, StepDefinition, WorkflowDefinition};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Ctx {
    log: Vec<String>,
}

impl WorkflowData for Ctx {
    fn workflow_type() -> &'static str {
        "test"
    }
}

fn engine() -> WorkflowEngine<Ctx, InMemoryStore<Ctx>> {
    WorkflowEngine::new()
}

async fn start(
    engine: &WorkflowEngine<Ctx, InMemoryStore<Ctx>>,
    wf: WorkflowDefinition<Ctx>,
) -> WorkflowRunId {
    engine.register_workflow(wf).unwrap();
    engine
        .start_workflow(WorkflowId::new("wf"), Ctx { log: vec![] }, "seed".into())
        .await
        .unwrap()
}

#[tokio::test]
async fn sleep_step_waits_and_completes() {
    let engine = engine();
    let wf = WorkflowDefinition::new("wf", "wf").add_step(
        StepDefinition::new("nap", "nap", noop_executor::<Ctx>())
            .with_mode(StepMode::Sleep { duration_secs: 1 }),
    );
    let run = start(&engine, wf).await;

    let start = std::time::Instant::now();
    engine
        .wait_for_completion(run, "wf", Duration::from_secs(10))
        .await
        .unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(900),
        "sleep fired too early: {elapsed:?}"
    );

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(
        state.step_states[&StepId::new("nap")].status,
        StepStatus::Succeeded
    );

    // Journal holds pending -> completed Sleep entries.
    let journal = engine.state_store().journal(run).await.unwrap();
    let sleeps: Vec<_> = journal
        .iter()
        .filter(|e| matches!(e, JournalEntry::Sleep { .. }))
        .collect();
    assert_eq!(sleeps.len(), 2, "pending + fired sleep entries");
    assert!(!sleeps[0].is_completed());
    assert!(sleeps[1].is_completed());
}

#[tokio::test]
async fn wait_event_resolves_via_complete_event() {
    let engine = engine();
    let wf = WorkflowDefinition::new("wf", "wf").add_step(
        StepDefinition::new("approve", "approve", noop_executor::<Ctx>()).with_mode(
            StepMode::WaitEvent {
                name: "approve".into(),
                timeout_secs: 10,
            },
        ),
    );
    let run = start(&engine, wf).await;

    // Let the step arm, then complete it from "elsewhere".
    tokio::time::sleep(Duration::from_millis(100)).await;
    engine
        .complete_event(run, "approve", EntryResult::Success(b"approved".to_vec()))
        .await
        .unwrap();

    engine
        .wait_for_completion(run, "wf", Duration::from_secs(10))
        .await
        .unwrap();
    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(
        state.step_states[&StepId::new("approve")].status,
        StepStatus::Succeeded
    );
}

#[tokio::test]
async fn wait_event_pre_delivery_before_wait() {
    let engine = engine();
    // A slow first step gives complete_event time to fire before the wait arms.
    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(StepDefinition::new(
            "slow",
            "slow",
            Arc::new(FunctionStep::new(|_: &mut WorkflowContext<Ctx>| {
                Box::pin(async {
                    tokio::time::sleep(Duration::from_millis(300)).await;
                    Ok(StepResult::Success)
                })
            })),
        ))
        .add_step(
            StepDefinition::new("approve", "approve", noop_executor::<Ctx>())
                .with_mode(StepMode::WaitEvent {
                    name: "approve".into(),
                    timeout_secs: 10,
                })
                .depends_on(&["slow"]),
        );
    let run = start(&engine, wf).await;

    // Fire before the wait step has armed (pre-delivery).
    tokio::time::sleep(Duration::from_millis(50)).await;
    engine
        .complete_event(run, "approve", EntryResult::Success(b"yes".to_vec()))
        .await
        .unwrap();

    engine
        .wait_for_completion(run, "wf", Duration::from_secs(10))
        .await
        .unwrap();
    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(
        state.step_states[&StepId::new("approve")].status,
        StepStatus::Succeeded
    );
}

#[tokio::test]
async fn concurrent_wait_events_same_name_both_resolve() {
    // Two independent steps waiting on the same event name must not collide:
    // a single `complete_event` call resolves both, not just whichever
    // registered its in-process waiter last.
    let engine = engine();
    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(
            StepDefinition::new("approve_a", "approve_a", noop_executor::<Ctx>()).with_mode(
                StepMode::WaitEvent {
                    name: "approve".into(),
                    timeout_secs: 10,
                },
            ),
        )
        .add_step(
            StepDefinition::new("approve_b", "approve_b", noop_executor::<Ctx>()).with_mode(
                StepMode::WaitEvent {
                    name: "approve".into(),
                    timeout_secs: 10,
                },
            ),
        );
    let run = start(&engine, wf).await;

    // Let both steps arm on the same event name before completing it once.
    tokio::time::sleep(Duration::from_millis(100)).await;
    engine
        .complete_event(run, "approve", EntryResult::Success(b"yes".to_vec()))
        .await
        .unwrap();

    engine
        .wait_for_completion(run, "wf", Duration::from_secs(10))
        .await
        .unwrap();
    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(
        state.step_states[&StepId::new("approve_a")].status,
        StepStatus::Succeeded
    );
    assert_eq!(
        state.step_states[&StepId::new("approve_b")].status,
        StepStatus::Succeeded
    );
}

#[tokio::test]
async fn wait_event_timeout_fails_step() {
    let engine = engine();
    let wf = WorkflowDefinition::new("wf", "wf").add_step(
        StepDefinition::new("approve", "approve", noop_executor::<Ctx>()).with_mode(
            StepMode::WaitEvent {
                name: "approve".into(),
                timeout_secs: 1,
            },
        ),
    );
    let run = start(&engine, wf).await;

    let out = engine
        .wait_for_completion(run, "wf", Duration::from_secs(10))
        .await;
    assert!(out.is_err());
    assert!(out.unwrap_err().contains("timed out"));

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Failed);
    assert_eq!(
        state.step_states[&StepId::new("approve")].status,
        StepStatus::Failed
    );
}
