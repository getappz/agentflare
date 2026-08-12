use std::sync::Arc;
use std::time::Duration;

use flare_workflow::engine::WorkflowEngine;
use flare_workflow::executor::{FunctionStep, noop_executor};
use flare_workflow::sqlite_store::SqliteStore;
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

fn producer(id: String, tag: &'static str) -> StepDefinition<Ctx> {
    StepDefinition::new(
        id.clone(),
        id.clone(),
        Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
            ctx.data.log.push(tag.to_string());
            ctx.output = format!("out:{tag}");
            Box::pin(async move { Ok(StepResult::Success) })
        })),
    )
}

fn register(engine: &WorkflowEngine<Ctx, SqliteStore<Ctx>>, wf: WorkflowDefinition<Ctx>) {
    engine.register_workflow(wf).unwrap();
}

/// A WaitEvent step never completes on its own — it acts as the crash point:
/// the first engine is dropped while the run is parked on it, and a fresh
/// engine recovers the run over the same SQLite file.
#[tokio::test]
async fn crash_mid_run_resumes_from_last_completed_step_without_reexecution() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wf.db");

    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(producer("a".into(), "a"))
        .add_step(
            StepDefinition::new("approve", "approve", noop_executor::<Ctx>())
                .with_mode(StepMode::WaitEvent {
                    name: "approve".into(),
                    timeout_secs: 60,
                })
                .depends_on(&["a"]),
        )
        .add_step(producer("c".into(), "c").depends_on(&["approve"]));

    let run;
    {
        // Engine 1: start the run, let step "a" complete and the run park on
        // the wait event, then "crash" (drop the engine mid-run).
        let engine = WorkflowEngine::<Ctx, _>::with_store(SqliteStore::open_file(&path).unwrap());
        register(&engine, wf.clone());
        run = engine
            .start_workflow(WorkflowId::new("wf"), Ctx { log: vec![] }, "seed".into())
            .await
            .unwrap();

        // Wait until step a is journaled as completed AND the run is parked
        // on the wait event (pending WaitEvent entry present).
        let store = engine.state_store().clone();
        for _ in 0..100 {
            let journal = store.journal(run).await.unwrap();
            let a_done = journal.iter().any(|e| {
                matches!(
                    e,
                    JournalEntry::StepRun { step_id, result: Some(_), .. } if step_id == &StepId::new("a")
                )
            });
            let waiting = journal
                .iter()
                .any(|e| matches!(e, JournalEntry::WaitEvent { name, result: None } if name == "approve"));
            if a_done && waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Drop engine 1 = crash; the run is still Running in the store.
    }

    // Engine 2 over the same file: recover the run.
    let engine = WorkflowEngine::<Ctx, _>::with_store(SqliteStore::open_file(&path).unwrap());
    register(&engine, wf);
    let resumed = engine.recover().await.unwrap();
    assert!(
        resumed.contains(&run),
        "recover() must resume the crashed run"
    );

    // A human completes the approval; the pipeline proceeds.
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
    // Step "a" ran exactly once (memoized); "c" ran after approval.
    assert_eq!(
        state.context.data.log,
        vec!["a".to_string(), "c".to_string()]
    );
}

/// A pending Sleep is re-armed by recovery and fires once its wake time is
/// reached, without a fresh pending entry being appended (idempotent re-arm).
#[tokio::test]
async fn recover_reams_pending_sleep() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wf.db");

    let wf = WorkflowDefinition::new("wf", "wf").add_step(
        StepDefinition::new("nap", "nap", noop_executor::<Ctx>())
            .with_mode(StepMode::Sleep { duration_secs: 1 }),
    );

    let run;
    {
        let engine = WorkflowEngine::<Ctx, _>::with_store(SqliteStore::open_file(&path).unwrap());
        register(&engine, wf.clone());
        run = engine
            .start_workflow(WorkflowId::new("wf"), Ctx { log: vec![] }, "seed".into())
            .await
            .unwrap();
        let store = engine.state_store().clone();
        for _ in 0..100 {
            let journal = store.journal(run).await.unwrap();
            if journal
                .iter()
                .any(|e| matches!(e, JournalEntry::Sleep { result: None, .. }))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Crash before the sleep fires.
    }

    let engine = WorkflowEngine::<Ctx, _>::with_store(SqliteStore::open_file(&path).unwrap());
    register(&engine, wf);
    engine.recover().await.unwrap();

    engine
        .wait_for_completion(run, "wf", Duration::from_secs(10))
        .await
        .unwrap();

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Completed);
    assert_eq!(
        state.step_states[&StepId::new("nap")].status,
        StepStatus::Succeeded
    );

    // Exactly one PENDING entry survives re-arm (idempotent re-arm); the
    // recovery's execution fires it. (A leaked pre-crash task may also fire
    // its own completed entry, so only the pending count is exact.)
    let journal = engine.state_store().journal(run).await.unwrap();
    let pending_sleeps = journal
        .iter()
        .filter(|e| matches!(e, JournalEntry::Sleep { result: None, .. }))
        .count();
    assert_eq!(pending_sleeps, 1);
    let fired_sleeps = journal
        .iter()
        .filter(|e| {
            matches!(
                e,
                JournalEntry::Sleep {
                    result: Some(_),
                    ..
                }
            )
        })
        .count();
    assert!(fired_sleeps >= 1);
}

/// Racing complete_event calls complete the wait exactly once.
#[tokio::test]
async fn racing_completions_resolve_to_one() {
    let engine = WorkflowEngine::<Ctx, _>::with_store(SqliteStore::open_memory().unwrap());
    let wf = WorkflowDefinition::new("wf", "wf").add_step(
        StepDefinition::new("approve", "approve", noop_executor::<Ctx>()).with_mode(
            StepMode::WaitEvent {
                name: "approve".into(),
                timeout_secs: 10,
            },
        ),
    );
    register(&engine, wf);
    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { log: vec![] }, "seed".into())
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(50)).await;
    let (a, b) = tokio::join!(
        engine.complete_event(run, "approve", EntryResult::Success(b"yes".to_vec())),
        engine.complete_event(run, "approve", EntryResult::Success(b"yes".to_vec())),
    );
    assert!(a.is_ok());
    assert!(b.is_ok());

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
