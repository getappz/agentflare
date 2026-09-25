//! `pause_workflow`/`resume_workflow`: a pause stops the driver at the next
//! step boundary, a step stopped mid-flight by the pause is neither failed
//! nor journaled, paused runs are left alone by `recover`, and a resume
//! continues from the same step with earlier steps memoized.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

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
        "pause-test"
    }
}

fn logging_step(id: &'static str) -> StepDefinition<Ctx> {
    StepDefinition::new(
        id,
        id,
        Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
            ctx.data.log.push(id.to_string());
            Box::pin(async move { Ok(StepResult::Success) })
        })),
    )
}

/// Stands in for an agent turn: runs until `kill` is set (the job's cancel
/// flag stopping the agent), then fails; succeeds straight away once
/// `kill_armed` is cleared.
fn agent_step(
    started: Arc<AtomicU32>,
    kill: Arc<AtomicBool>,
    kill_armed: Arc<AtomicBool>,
) -> StepDefinition<Ctx> {
    StepDefinition::new(
        "agent",
        "agent",
        Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
            started.fetch_add(1, Ordering::SeqCst);
            let kill = kill.clone();
            let armed = kill_armed.load(Ordering::SeqCst);
            ctx.data.log.push("agent".to_string());
            Box::pin(async move {
                if !armed {
                    return Ok(StepResult::Success);
                }
                while !kill.load(Ordering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(WorkflowError::Store("agent killed".into()))
            })
        })),
    )
    .depends_on(&["setup"])
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

#[tokio::test]
async fn pause_stops_at_the_step_boundary_and_resume_continues_from_the_same_step() {
    let started = Arc::new(AtomicU32::new(0));
    let kill = Arc::new(AtomicBool::new(false));
    let kill_armed = Arc::new(AtomicBool::new(true));
    let definition = WorkflowDefinition::new("wf", "wf")
        .add_step(logging_step("setup"))
        .add_step(agent_step(
            started.clone(),
            kill.clone(),
            kill_armed.clone(),
        ))
        .add_step(logging_step("finish").depends_on(&["agent"]));

    let store = InMemoryStore::<Ctx>::new();
    let engine = WorkflowEngine::with_store(store.clone());
    engine.register_workflow(definition).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new("wf"), Ctx::default(), String::new())
        .await
        .unwrap();

    wait_for("agent step to start", || {
        started.load(Ordering::SeqCst) == 1
    })
    .await;
    assert!(engine.pause_workflow(run_id).await.unwrap());
    // The pause kills the in-flight agent turn, as the job cancel flag does.
    kill.store(true, Ordering::SeqCst);
    wait_for("driver to stop", || !engine.is_driving(run_id)).await;

    let state = engine.get_status(run_id).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Paused);
    assert_eq!(
        state.step_states[&StepId::new("agent")].status,
        StepStatus::Pending,
        "the stopped step must not read as failed"
    );
    let journal = store.journal(run_id).await.unwrap();
    assert!(
        !journal.iter().any(|e| matches!(
            e,
            JournalEntry::StepRun {
                result: Some(EntryResult::Failure { .. }),
                ..
            }
        )),
        "a paused step's failure must not be journaled: {journal:?}"
    );
    // Paused runs are not auto-recovered at boot.
    assert!(engine.recover().await.unwrap().is_empty());
    assert_eq!(
        engine.get_status(run_id).await.unwrap().status,
        WorkflowStatus::Paused
    );
    assert!(
        !engine.pause_workflow(run_id).await.unwrap(),
        "already paused"
    );

    kill_armed.store(false, Ordering::SeqCst);
    assert!(engine.resume_workflow(run_id).await.unwrap());
    wait_for("run to complete", || !engine.is_driving(run_id)).await;
    let state = engine.get_status(run_id).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed, "{state:?}");
    assert_eq!(started.load(Ordering::SeqCst), 2, "agent re-run once");
    let log = state.context.data.log;
    assert_eq!(
        log.iter().filter(|s| *s == "setup").count(),
        1,
        "completed steps stay memoized across the pause: {log:?}"
    );
    assert_eq!(log.last().map(String::as_str), Some("finish"));
    assert!(!engine.resume_workflow(run_id).await.unwrap(), "not paused");
}

#[tokio::test]
async fn cancel_after_pause_is_terminal() {
    let started = Arc::new(AtomicU32::new(0));
    let kill = Arc::new(AtomicBool::new(false));
    let definition = WorkflowDefinition::new("wf", "wf")
        .add_step(logging_step("setup"))
        .add_step(agent_step(
            started.clone(),
            kill.clone(),
            Arc::new(AtomicBool::new(true)),
        ));
    let engine = WorkflowEngine::with_store(InMemoryStore::<Ctx>::new());
    engine.register_workflow(definition).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new("wf"), Ctx::default(), String::new())
        .await
        .unwrap();
    wait_for("agent step to start", || {
        started.load(Ordering::SeqCst) == 1
    })
    .await;
    assert!(engine.pause_workflow(run_id).await.unwrap());
    kill.store(true, Ordering::SeqCst);
    wait_for("driver to stop", || !engine.is_driving(run_id)).await;

    engine.cancel_workflow(run_id).await.unwrap();
    assert_eq!(
        engine.get_status(run_id).await.unwrap().status,
        WorkflowStatus::Cancelled
    );
    assert!(!engine.resume_workflow(run_id).await.unwrap());
    assert!(!engine.resume_run(run_id).await.unwrap());
    assert!(!engine.pause_workflow(run_id).await.unwrap());
}

/// The work-item pipeline's agent step is a `Loop`: a pause mid-iteration
/// must keep the journaled iterations and re-run only the stopped one.
#[tokio::test]
async fn pause_inside_a_loop_step_resumes_from_the_stopped_iteration() {
    let iterations = Arc::new(AtomicU32::new(0));
    let kill = Arc::new(AtomicBool::new(false));
    let kill_armed = Arc::new(AtomicBool::new(true));
    let step = {
        let iterations = iterations.clone();
        let kill = kill.clone();
        let kill_armed = kill_armed.clone();
        StepDefinition::new(
            "loop",
            "loop",
            Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
                let n = iterations.fetch_add(1, Ordering::SeqCst) + 1;
                let block = n == 2 && kill_armed.load(Ordering::SeqCst);
                let kill = kill.clone();
                ctx.data.log.push(format!("iter{n}"));
                ctx.output = if n >= 3 { "DONE".into() } else { "more".into() };
                Box::pin(async move {
                    if block {
                        while !kill.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        return Err(WorkflowError::Store("agent killed".into()));
                    }
                    Ok(StepResult::Success)
                })
            })),
        )
        .with_mode(StepMode::Loop {
            max_iterations: 5,
            until: "DONE".into(),
        })
    };
    let store = InMemoryStore::<Ctx>::new();
    let engine = WorkflowEngine::with_store(store.clone());
    engine
        .register_workflow(WorkflowDefinition::new("wf", "wf").add_step(step))
        .unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new("wf"), Ctx::default(), String::new())
        .await
        .unwrap();
    wait_for("second iteration", || {
        iterations.load(Ordering::SeqCst) == 2
    })
    .await;
    assert!(engine.pause_workflow(run_id).await.unwrap());
    kill.store(true, Ordering::SeqCst);
    wait_for("driver to stop", || !engine.is_driving(run_id)).await;
    assert_eq!(
        engine.get_status(run_id).await.unwrap().status,
        WorkflowStatus::Paused
    );
    assert_eq!(
        iterations.load(Ordering::SeqCst),
        2,
        "no retry while paused"
    );

    kill_armed.store(false, Ordering::SeqCst);
    assert!(engine.resume_workflow(run_id).await.unwrap());
    wait_for("run to complete", || !engine.is_driving(run_id)).await;
    let state = engine.get_status(run_id).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed, "{state:?}");
    let journal = store.journal(run_id).await.unwrap();
    let journaled: Vec<u32> = journal
        .iter()
        .filter_map(|e| match e {
            JournalEntry::LoopIteration { iteration, .. } => Some(*iteration),
            _ => None,
        })
        .collect();
    // Three executions: iteration 1, the stopped iteration 2, and its re-run
    // (which reports DONE) -- iteration 1 is never repeated.
    assert_eq!(iterations.load(Ordering::SeqCst), 3);
    assert_eq!(journaled, vec![1, 2], "iteration 1 kept, 2 re-run once");
}
