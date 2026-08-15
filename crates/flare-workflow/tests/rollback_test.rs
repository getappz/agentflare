use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use flare_workflow::engine::WorkflowEngine;
use flare_workflow::executor::{FunctionStep, StepExecutor, noop_executor};
use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::store::InMemoryStore;
use flare_workflow::types::*;
use flare_workflow::{
    EntryResult, StateStore, StepDefinition, ValidationError, WorkflowDefinition,
};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct Ctx {
    log: Vec<String>,
}

impl WorkflowData for Ctx {
    fn workflow_type() -> &'static str {
        "test"
    }
}

/// A step that succeeds, producing `out:{id}` as its output.
fn producer(id: &str) -> StepDefinition<Ctx> {
    let id_s = id.to_string();
    StepDefinition::new(
        id,
        id,
        Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
            let out = format!("out:{id_s}");
            Box::pin(async move {
                ctx.output = out;
                Ok(StepResult::Success)
            })
        })),
    )
}

/// A step that always fails terminally on its first attempt (no retries).
fn failing(id: &str) -> StepDefinition<Ctx> {
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

/// A rollback handler that records `(step_id, context.output)` into a shared
/// log, in invocation order, and always succeeds.
fn recording_rollback(
    log: Arc<Mutex<Vec<(String, String)>>>,
    id: &str,
) -> Arc<dyn StepExecutor<Ctx>> {
    let id_s = id.to_string();
    Arc::new(FunctionStep::new(move |ctx: &mut WorkflowContext<Ctx>| {
        let log = Arc::clone(&log);
        let id_s = id_s.clone();
        let out = ctx.output.clone();
        Box::pin(async move {
            log.lock().unwrap().push((id_s, out));
            Ok(StepResult::Success)
        })
    }))
}

/// (a) + (b): a later step fails; earlier steps' rollbacks fire in reverse
/// start order, and the failing step's own rollback runs with `output: ""`
/// (no successful `StepRun` snapshot exists for it to reconstruct from).
#[tokio::test]
async fn rollbacks_fire_in_reverse_start_order_with_empty_output_for_trigger() {
    let engine = WorkflowEngine::<Ctx, InMemoryStore<Ctx>>::new();
    let log: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(producer("a").with_rollback(recording_rollback(Arc::clone(&log), "a"), None))
        .add_step(
            producer("b")
                .depends_on(&["a"])
                .with_rollback(recording_rollback(Arc::clone(&log), "b"), None),
        )
        .add_step(
            failing("c")
                .depends_on(&["b"])
                .with_rollback(recording_rollback(Arc::clone(&log), "c"), None),
        );
    engine.register_workflow(wf).unwrap();

    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { log: vec![] }, "in".into())
        .await
        .unwrap();
    let out = engine
        .wait_for_completion(run, "wf", Duration::from_secs(5))
        .await;
    assert!(out.is_err(), "workflow with a terminal failure must fail");

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Failed);

    let recorded = log.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            ("c".to_string(), String::new()),
            ("b".to_string(), "out:b".to_string()),
            ("a".to_string(), "out:a".to_string()),
        ],
        "rollbacks must fire in reverse step-start order, and the failing \
         step's own rollback must see an empty output"
    );

    // Every rollback is journaled as completed, for audit + memoization.
    let journal = engine.state_store().journal(run).await.unwrap();
    for id in ["a", "b", "c"] {
        assert!(
            journal.iter().any(|e| matches!(
                e,
                JournalEntry::Rollback { step_id, result: Some(EntryResult::Success(_)), .. }
                    if step_id == &StepId::new(id)
            )),
            "step '{id}' should have a completed Rollback journal entry"
        );
    }
}

/// (d): a rollback handler that exhausts its retries logs the failure and
/// continues unwinding the remaining (earlier) steps rather than aborting.
#[tokio::test]
async fn exhausted_rollback_retries_do_not_abort_the_sweep() {
    let engine = WorkflowEngine::<Ctx, InMemoryStore<Ctx>>::new();
    let log: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let x_rollback_attempts = Arc::new(AtomicU32::new(0));

    let always_fails_rollback: Arc<dyn StepExecutor<Ctx>> = {
        let attempts = Arc::clone(&x_rollback_attempts);
        Arc::new(FunctionStep::new(move |_ctx: &mut WorkflowContext<Ctx>| {
            let attempts = Arc::clone(&attempts);
            Box::pin(async move {
                attempts.fetch_add(1, Ordering::SeqCst);
                Err(WorkflowError::StepFailed {
                    step_id: StepId::new("x"),
                    message: "compensation always fails".into(),
                })
            })
        }))
    };
    let succeeding_rollback: Arc<dyn StepExecutor<Ctx>> = {
        let log = Arc::clone(&log);
        Arc::new(FunctionStep::new(move |_ctx: &mut WorkflowContext<Ctx>| {
            let log = Arc::clone(&log);
            Box::pin(async move {
                log.lock().unwrap().push("w".to_string());
                Ok(StepResult::Success)
            })
        }))
    };

    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(producer("w").with_rollback(succeeding_rollback, None))
        .add_step(producer("x").depends_on(&["w"]).with_rollback(
            always_fails_rollback,
            Some(RetryPolicy {
                max_attempts: 2,
                backoff: BackoffStrategy::Fixed(Duration::from_millis(1)),
            }),
        ))
        .add_step(failing("y").depends_on(&["x"]));
    engine.register_workflow(wf).unwrap();

    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { log: vec![] }, "in".into())
        .await
        .unwrap();
    let out = engine
        .wait_for_completion(run, "wf", Duration::from_secs(5))
        .await;
    assert!(out.is_err());

    // x's handler exhausted its 2 configured attempts...
    assert_eq!(x_rollback_attempts.load(Ordering::SeqCst), 2);
    // ...but w's rollback (declared earlier, unwound after x) still ran.
    assert_eq!(log.lock().unwrap().clone(), vec!["w".to_string()]);

    let journal = engine.state_store().journal(run).await.unwrap();
    assert!(journal.iter().any(|e| matches!(
        e,
        JournalEntry::Rollback { step_id, result: Some(EntryResult::Failure { .. }), .. }
            if step_id == &StepId::new("x")
    )));
    assert!(journal.iter().any(|e| matches!(
        e,
        JournalEntry::Rollback { step_id, result: Some(EntryResult::Success(_)), .. }
            if step_id == &StepId::new("w")
    )));

    let state = engine.get_status(run).await.unwrap();
    let error = state.error.unwrap_or_default();
    assert!(
        error.contains("rollback failed for steps: x"),
        "workflow error should record which rollback(s) failed: {error}"
    );
}

/// A step that blocks for a while before succeeding, giving a test room to
/// cancel the run while it's in flight.
fn slow(id: &str) -> StepDefinition<Ctx> {
    StepDefinition::new(
        id,
        id,
        Arc::new(FunctionStep::new(|_ctx: &mut WorkflowContext<Ctx>| {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_secs(3)).await;
                Ok(StepResult::Success)
            })
        })),
    )
}

/// Cloudflare `instance.terminate({ rollback: true })` parity: explicitly
/// cancelling a run with already-succeeded steps compensates them in reverse
/// start order, same as the natural-failure path.
#[tokio::test]
async fn cancel_with_rollback_compensates_succeeded_steps() {
    let engine = WorkflowEngine::<Ctx, InMemoryStore<Ctx>>::new();
    let log: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(producer("a").with_rollback(recording_rollback(Arc::clone(&log), "a"), None))
        .add_step(
            producer("b")
                .depends_on(&["a"])
                .with_rollback(recording_rollback(Arc::clone(&log), "b"), None),
        )
        .add_step(slow("c").depends_on(&["b"]));
    engine.register_workflow(wf).unwrap();

    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { log: vec![] }, "in".into())
        .await
        .unwrap();

    // Wait until a and b have both durably succeeded (c is now the one
    // in flight, blocked on its 3s sleep) before cancelling.
    for _ in 0..200 {
        let journal = engine.state_store().journal(run).await.unwrap();
        let done = |id: &str| {
            journal.iter().any(|e| {
                matches!(
                    e,
                    JournalEntry::StepRun { step_id, result: Some(EntryResult::Success(_)), .. }
                        if step_id == &StepId::new(id)
                )
            })
        };
        if done("a") && done("b") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    engine.cancel_workflow_with_rollback(run).await.unwrap();

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Cancelled);

    let recorded = log.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![
            ("b".to_string(), "out:b".to_string()),
            ("a".to_string(), "out:a".to_string()),
        ],
        "explicit cancellation should compensate already-succeeded steps in reverse start order"
    );

    let journal = engine.state_store().journal(run).await.unwrap();
    for id in ["a", "b"] {
        assert!(
            journal.iter().any(|e| matches!(
                e,
                JournalEntry::Rollback { step_id, result: Some(EntryResult::Success(_)), .. }
                    if step_id == &StepId::new(id)
            )),
            "step '{id}' should have a completed Rollback journal entry"
        );
    }
}

/// Plain `cancel_workflow` (no rollback requested) must not compensate
/// succeeded steps — this is the pre-existing behavior `cancel_workflow_with_rollback`
/// is opt-in on top of.
#[tokio::test]
async fn plain_cancel_does_not_trigger_rollback() {
    let engine = WorkflowEngine::<Ctx, InMemoryStore<Ctx>>::new();
    let log: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));

    let wf = WorkflowDefinition::new("wf", "wf")
        .add_step(producer("a").with_rollback(recording_rollback(Arc::clone(&log), "a"), None))
        .add_step(slow("c").depends_on(&["a"]));
    engine.register_workflow(wf).unwrap();

    let run = engine
        .start_workflow(WorkflowId::new("wf"), Ctx { log: vec![] }, "in".into())
        .await
        .unwrap();

    for _ in 0..200 {
        let journal = engine.state_store().journal(run).await.unwrap();
        let a_done = journal.iter().any(|e| {
            matches!(
                e,
                JournalEntry::StepRun { step_id, result: Some(EntryResult::Success(_)), .. }
                    if step_id == &StepId::new("a")
            )
        });
        if a_done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    engine.cancel_workflow(run).await.unwrap();

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Cancelled);
    assert!(
        log.lock().unwrap().is_empty(),
        "plain cancel_workflow must not compensate succeeded steps"
    );
}

/// (e): registering a rollback handler on an unsupported step mode
/// (`Collect`/`Sleep`/`WaitEvent`) is rejected at `register_workflow` time.
#[tokio::test]
async fn rollback_on_unsupported_mode_rejected_at_registration() {
    let engine = WorkflowEngine::<Ctx, InMemoryStore<Ctx>>::new();

    for (mode, mode_name) in [
        (StepMode::Sleep { duration_secs: 1 }, "sleep"),
        (
            StepMode::WaitEvent {
                name: "approve".into(),
                timeout_secs: 1,
            },
            "wait_event",
        ),
        (StepMode::Collect, "collect"),
    ] {
        let step = StepDefinition::new("s", "s", noop_executor::<Ctx>())
            .with_mode(mode)
            .with_rollback(noop_executor::<Ctx>(), None);
        let wf = WorkflowDefinition::new("wf", "wf").add_step(step);
        let err = engine.register_workflow(wf).unwrap_err();
        match err {
            ValidationError::RollbackUnsupportedMode { step, mode } => {
                assert_eq!(step, StepId::new("s"));
                assert_eq!(mode, mode_name);
            }
            other => panic!("expected RollbackUnsupportedMode for {mode_name}, got {other:?}"),
        }
    }
}

/// (c): a crash mid-rollback-phase (one handler never returns) followed by
/// `recover()` compensates the remaining steps exactly once — already
/// -compensated steps are never re-invoked.
#[tokio::test]
async fn crash_mid_rollback_then_recover_compensates_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wf.db");

    let q_calls = Arc::new(AtomicU32::new(0));
    let r_calls = Arc::new(AtomicU32::new(0));
    let p_hang_started = Arc::new(AtomicU32::new(0));
    let p_real_calls = Arc::new(AtomicU32::new(0));

    fn counting_rollback(counter: Arc<AtomicU32>) -> Arc<dyn StepExecutor<Ctx>> {
        Arc::new(FunctionStep::new(move |_ctx: &mut WorkflowContext<Ctx>| {
            let counter = Arc::clone(&counter);
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(StepResult::Success)
            })
        }))
    }

    fn hanging_rollback(started: Arc<AtomicU32>) -> Arc<dyn StepExecutor<Ctx>> {
        Arc::new(FunctionStep::new(move |_ctx: &mut WorkflowContext<Ctx>| {
            let started = Arc::clone(&started);
            Box::pin(async move {
                started.fetch_add(1, Ordering::SeqCst);
                // Simulates a crash: this rollback attempt never returns, so
                // no Rollback entry is ever journaled for it by this engine.
                std::future::pending::<()>().await;
                unreachable!()
            })
        }))
    }

    // p -> q -> r (sequential), r fails terminally. Reverse-start order for
    // the unwind is r, q, p — the engine gets stuck on p (the last one
    // processed) while r and q have already been durably compensated.
    let wf_crash = WorkflowDefinition::new("wf", "wf")
        .add_step(producer("p").with_rollback(hanging_rollback(Arc::clone(&p_hang_started)), None))
        .add_step(
            producer("q")
                .depends_on(&["p"])
                .with_rollback(counting_rollback(Arc::clone(&q_calls)), None),
        )
        .add_step(
            failing("r")
                .depends_on(&["q"])
                .with_rollback(counting_rollback(Arc::clone(&r_calls)), None),
        );

    let run;
    {
        let engine = WorkflowEngine::<Ctx, _>::with_store(SqliteStore::open_file(&path).unwrap());
        engine.register_workflow(wf_crash).unwrap();
        run = engine
            .start_workflow(WorkflowId::new("wf"), Ctx { log: vec![] }, "in".into())
            .await
            .unwrap();

        // Wait until r and q are both durably compensated and p's hang
        // handler has started (i.e. the unwind is now stuck on p).
        let store = engine.state_store().clone();
        for _ in 0..200 {
            let journal = store.journal(run).await.unwrap();
            let r_done = journal.iter().any(|e| matches!(
                e,
                JournalEntry::Rollback { step_id, result: Some(_), .. } if step_id == &StepId::new("r")
            ));
            let q_done = journal.iter().any(|e| matches!(
                e,
                JournalEntry::Rollback { step_id, result: Some(_), .. } if step_id == &StepId::new("q")
            ));
            if r_done && q_done && p_hang_started.load(Ordering::SeqCst) >= 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(r_calls.load(Ordering::SeqCst), 1);
        assert_eq!(q_calls.load(Ordering::SeqCst), 1);
        assert_eq!(p_hang_started.load(Ordering::SeqCst), 1);

        // The run must still read Running: status only flips to Failed
        // after the rollback phase returns, which it hasn't (stuck on p).
        let state = engine.get_status(run).await.unwrap();
        assert_eq!(state.status, WorkflowStatus::Running);

        // "Crash": drop this engine handle. Its rollback-phase task is
        // orphaned mid-`p` and never makes further progress.
    }

    // Recovery: fresh engine over the same file, with a *working* rollback
    // handler for p (the process restarting with the same code — unlike the
    // permanently-hung in-flight call, a fresh attempt can complete).
    let wf_recovered = WorkflowDefinition::new("wf", "wf")
        .add_step(producer("p").with_rollback(counting_rollback(Arc::clone(&p_real_calls)), None))
        .add_step(
            producer("q")
                .depends_on(&["p"])
                .with_rollback(counting_rollback(Arc::clone(&q_calls)), None),
        )
        .add_step(
            failing("r")
                .depends_on(&["q"])
                .with_rollback(counting_rollback(Arc::clone(&r_calls)), None),
        );
    let engine = WorkflowEngine::<Ctx, _>::with_store(SqliteStore::open_file(&path).unwrap());
    engine.register_workflow(wf_recovered).unwrap();
    let resumed = engine.recover().await.unwrap();
    assert!(resumed.contains(&run));

    for _ in 0..200 {
        let state = engine.get_status(run).await.unwrap();
        if state.status == WorkflowStatus::Failed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let state = engine.get_status(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Failed);

    // Exactly-once: q and r's rollback handlers (already compensated before
    // the crash) were never re-invoked by recovery; p's fresh handler ran
    // exactly once.
    assert_eq!(q_calls.load(Ordering::SeqCst), 1);
    assert_eq!(r_calls.load(Ordering::SeqCst), 1);
    assert_eq!(p_real_calls.load(Ordering::SeqCst), 1);

    let journal = engine.state_store().journal(run).await.unwrap();
    for id in ["p", "q", "r"] {
        let completed_count = journal
            .iter()
            .filter(|e| {
                matches!(
                    e,
                    JournalEntry::Rollback { step_id, result: Some(_), .. }
                        if step_id == &StepId::new(id)
                )
            })
            .count();
        assert_eq!(
            completed_count, 1,
            "step '{id}' must have exactly one completed Rollback entry"
        );
    }
}
