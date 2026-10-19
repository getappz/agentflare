use chrono::Utc;
use flare_workflow::budget::{BudgetLimits, BudgetOperation, BudgetState};
use flare_workflow::{
    PipelineData, SqliteStore, StateStore, WorkflowId, WorkflowRunId, WorkflowState, WorkflowStatus,
};

fn limits() -> BudgetLimits {
    BudgetLimits {
        max_cost_usd: Some(1.0),
        reserve_per_call_usd: Some(0.6),
        max_invocations: Some(3),
        max_elapsed_secs: None,
    }
}

fn reserve() -> BudgetOperation {
    BudgetOperation::Reserve {
        step_id: "solve".into(),
        agent: "fixture".into(),
        logical_attempt: 1,
    }
}

async fn seed(store: &SqliteStore<PipelineData>, policy: BudgetLimits) -> WorkflowRunId {
    let run = WorkflowRunId::new();
    let mut state = WorkflowState::new(run, WorkflowId::new("test"), PipelineData);
    state.status = WorkflowStatus::Running;
    state.budget = Some(BudgetState::new(policy, Utc::now()).unwrap());
    store.save(state).await.unwrap();
    run
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reservations_are_shared_across_independent_connections_and_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workflow.db");
    let first = SqliteStore::open_file(&path).unwrap();
    let second = SqliteStore::<PipelineData>::open_file(&path).unwrap();
    let run = seed(&first, limits()).await;
    let (a, b) = tokio::join!(
        first.budget_operation(run, reserve()),
        second.budget_operation(run, reserve())
    );
    let a = a.unwrap().reservation.unwrap();
    let b = b.unwrap().reservation.unwrap();
    assert_eq!(
        a.reserved_micro_usd.unwrap() + b.reserved_micro_usd.unwrap(),
        1_000_000
    );
    assert_ne!(a.id, b.id);
    assert!(first.budget_operation(run, reserve()).await.is_err());
    drop(first);
    drop(second);
    let reopened = SqliteStore::<PipelineData>::open_file(&path).unwrap();
    let snapshot = reopened
        .budget_operation(run, BudgetOperation::Snapshot)
        .await
        .unwrap()
        .state;
    assert_eq!(snapshot.summary()["held_cost_usd"], 1.0);
    assert_eq!(snapshot.summary()["unknown_usage_count"], 2);
    assert!(reopened.budget_operation(run, reserve()).await.is_err());
}

#[tokio::test]
async fn reconciliation_releases_only_reported_unused_cost_and_counts_retries() {
    let store = SqliteStore::open_memory().unwrap();
    let run = seed(&store, limits()).await;
    let first = store
        .budget_operation(run, reserve())
        .await
        .unwrap()
        .reservation
        .unwrap();
    store
        .budget_operation(
            run,
            BudgetOperation::Reconcile {
                id: first.id,
                cost_usd: Some(0.1),
            },
        )
        .await
        .unwrap();
    store.budget_operation(run, reserve()).await.unwrap();
    store.budget_operation(run, reserve()).await.unwrap();
    assert!(store.budget_operation(run, reserve()).await.is_err());
    let summary = store
        .budget_operation(run, BudgetOperation::Snapshot)
        .await
        .unwrap()
        .state
        .summary();
    assert_eq!(summary["reported_cost_usd"], 0.1);
    assert_eq!(summary["held_cost_usd"], 0.9);
    assert_eq!(summary["invocation_reservations"], 3);
}

#[tokio::test]
async fn stale_step_state_cannot_refill_the_ledger_and_missing_usage_is_not_zero() {
    let store = SqliteStore::open_memory().unwrap();
    let run = seed(&store, limits()).await;
    let stale = store.load(run).await.unwrap();
    let first = store
        .budget_operation(run, reserve())
        .await
        .unwrap()
        .reservation
        .unwrap();
    store
        .budget_operation(
            run,
            BudgetOperation::Reconcile {
                id: first.id,
                cost_usd: None,
            },
        )
        .await
        .unwrap();
    store.save(stale).await.unwrap();
    let snapshot = store
        .budget_operation(run, BudgetOperation::Snapshot)
        .await
        .unwrap()
        .state;
    assert_eq!(snapshot.summary()["held_cost_usd"], 0.6);
    assert_eq!(snapshot.summary()["unknown_usage_count"], 1);
}

#[test]
fn invalid_limits_and_expired_deadlines_are_rejected() {
    for bad in [f64::NAN, f64::INFINITY, -1.0, 0.0, 0.00000001, 1e30] {
        let mut policy = limits();
        policy.max_cost_usd = Some(bad);
        assert!(BudgetState::new(policy, Utc::now()).is_err());
    }
    let mut policy = limits();
    policy.max_elapsed_secs = Some(1);
    let mut state = BudgetState::new(policy, Utc::now() - chrono::Duration::seconds(2)).unwrap();
    assert!(state.apply(reserve()).is_err());
}

#[test]
fn reported_overruns_block_new_calls_and_reconciliation_is_idempotent() {
    let mut state = BudgetState::new(limits(), Utc::now()).unwrap();
    let id = state.apply(reserve()).unwrap().reservation.unwrap().id;
    state
        .apply(BudgetOperation::Reconcile {
            id,
            cost_usd: Some(0.7),
        })
        .unwrap();
    state
        .apply(BudgetOperation::Reconcile {
            id,
            cost_usd: Some(0.7),
        })
        .unwrap();
    assert!(state.overrun());
    assert!(state.apply(reserve()).is_err());
    assert!(
        state
            .apply(BudgetOperation::Reconcile {
                id,
                cost_usd: Some(0.0)
            })
            .is_err()
    );
}

use flare_workflow::{JsonWorkflow, StepState, WorkflowEngine};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

fn workflow(value: serde_json::Value) -> (JsonWorkflow, BudgetLimits) {
    let json: JsonWorkflow = serde_json::from_value(value).unwrap();
    let limits = json.budget.clone().unwrap();
    (json, limits)
}

#[tokio::test]
async fn fanout_calls_share_one_allowance_before_any_reply_arrives() {
    let (json, _) = workflow(
        serde_json::json!({"name":"fanout", "budget":{"max_cost_usd":1.0,"reserve_per_call_usd":0.6},"steps":[
            {"name":"a","agent":"fixture","mode":"fan_out"},
            {"name":"b","agent":"fixture","mode":"fan_out"},
            {"name":"c","agent":"fixture","mode":"fan_out"}
        ]}),
    );
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let engine = WorkflowEngine::with_store(store.clone());
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let send: flare_workflow::SendMessage = Arc::new(move |inv| {
        let count = count.clone();
        Box::pin(async move {
            count.fetch_add(1, Ordering::SeqCst);
            let allocation = inv.allocation.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            allocation.record_cost(allocation.max_cost_usd());
            Ok(("done".into(), 1, 1))
        })
    });
    let definition = flare_workflow::compile_workflow(&json, send).unwrap();
    engine.register_workflow(definition).unwrap();
    let run = engine
        .start_workflow(WorkflowId::new("fanout"), PipelineData, String::new())
        .await
        .unwrap();
    assert!(
        engine
            .wait_for_completion(run, "test", Duration::from_secs(5))
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let summary = store
        .budget_operation(run, BudgetOperation::Snapshot)
        .await
        .unwrap()
        .state
        .summary();
    assert_eq!(summary["reported_cost_usd"], 1.0);
    assert_eq!(summary["remaining_cost_usd"], 0.0);
}

#[tokio::test]
async fn failed_calls_and_loop_iterations_consume_the_same_invocation_limit() {
    for looping in [false, true] {
        let mode = if looping {
            serde_json::json!({"loop":{"max_iterations":5,"until":"stop"}})
        } else {
            serde_json::json!("sequential")
        };
        let (json, _) = workflow(
            serde_json::json!({"name":"bounded","budget":{"max_invocations":2},"steps":[
                {"name":"solve","agent":"fixture","mode":mode,"error_mode":"retry","max_retries":5}
            ]}),
        );
        let store = SqliteStore::<PipelineData>::open_memory().unwrap();
        let engine = WorkflowEngine::with_store(store.clone()).with_jitter(0.0);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let send: flare_workflow::SendMessage = Arc::new(move |_| {
            count.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                if looping {
                    Ok(("continue".into(), 0, 0))
                } else {
                    Err("transport failed".into())
                }
            })
        });
        engine
            .register_workflow(flare_workflow::compile_workflow(&json, send).unwrap())
            .unwrap();
        let run = engine
            .start_workflow(WorkflowId::new("bounded"), PipelineData, String::new())
            .await
            .unwrap();
        assert!(
            engine
                .wait_for_completion(run, "test", Duration::from_secs(10))
                .await
                .is_err()
        );
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let state = store.load(run).await.unwrap();
        assert_eq!(state.status, WorkflowStatus::Failed);
        assert!(
            state
                .error
                .unwrap()
                .contains("invocation allowance exhausted")
        );
    }
}

#[tokio::test]
async fn recovery_cannot_reset_an_already_reserved_allowance() {
    let (json, policy) = workflow(
        serde_json::json!({"name":"test","budget":{"max_invocations":1},"steps":[{"name":"solve","agent":"fixture"}]}),
    );
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let run = seed(&store, policy).await;
    store.budget_operation(run, reserve()).await.unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let count = calls.clone();
    let send: flare_workflow::SendMessage = Arc::new(move |_| {
        count.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(("should not run".into(), 0, 0)) })
    });
    let definition = flare_workflow::compile_workflow(&json, send).unwrap();
    store
        .update(run, |state| {
            state.resume_identity = definition.run_identity(&PipelineData).unwrap();
            state
                .step_states
                .insert(flare_workflow::StepId::new("solve"), StepState::default());
        })
        .await
        .unwrap();
    let engine = WorkflowEngine::with_store(store.clone());
    engine.register_workflow(definition).unwrap();
    assert!(engine.resume_run(run).await.unwrap());
    assert!(
        engine
            .wait_for_completion(run, "test", Duration::from_secs(5))
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn the_run_deadline_also_cancels_a_durable_wait() {
    let (json, _) = workflow(
        serde_json::json!({"name":"deadline","budget":{"max_elapsed_secs":1},"steps":[
            {"name":"sleep","agent":"fixture","mode":{"sleep":{"duration_secs":30}}}
        ]}),
    );
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let engine = WorkflowEngine::with_store(store.clone());
    let send: flare_workflow::SendMessage =
        Arc::new(|_| Box::pin(async { panic!("sleep must not dispatch") }));
    engine
        .register_workflow(flare_workflow::compile_workflow(&json, send).unwrap())
        .unwrap();
    let run = engine
        .start_workflow(WorkflowId::new("deadline"), PipelineData, String::new())
        .await
        .unwrap();
    assert!(
        engine
            .wait_for_completion(run, "test", Duration::from_secs(5))
            .await
            .is_err()
    );
    let state = store.load(run).await.unwrap();
    assert_eq!(state.status, WorkflowStatus::Cancelled);
    assert!(state.error.unwrap().contains("elapsed allowance exhausted"));
}

#[tokio::test]
async fn deadline_drops_detached_native_steps_before_their_late_side_effects() {
    use flare_workflow::{FunctionStep, StepDefinition, StepResult, WorkflowDefinition};
    let wrote = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let late_write = wrote.clone();
    let step = StepDefinition::new(
        "late-write",
        "late-write",
        Arc::new(FunctionStep::new(
            move |_: &mut flare_workflow::WorkflowContext<PipelineData>| {
                let late_write = late_write.clone();
                Box::pin(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    late_write.store(true, Ordering::SeqCst);
                    Ok(StepResult::Success)
                })
            },
        )),
    );
    let engine = WorkflowEngine::<PipelineData, _>::with_store(SqliteStore::open_memory().unwrap());
    engine
        .register_workflow(
            WorkflowDefinition::new("deadline", "deadline")
                .with_budget(BudgetLimits {
                    max_elapsed_secs: Some(1),
                    ..Default::default()
                })
                .add_step(step),
        )
        .unwrap();
    let run = engine
        .start_workflow(WorkflowId::new("deadline"), PipelineData, String::new())
        .await
        .unwrap();
    assert!(
        engine
            .wait_for_completion(run, "test", Duration::from_secs(5))
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(1300)).await;
    assert!(!wrote.load(Ordering::SeqCst));
}

#[test]
fn budgeted_native_runs_have_an_identity_older_executors_will_reject() {
    let definition = flare_workflow::WorkflowDefinition::<PipelineData>::new("native", "native");
    assert_eq!(definition.run_identity(&PipelineData).unwrap(), None);
    let budgeted = definition.with_budget(BudgetLimits {
        max_invocations: Some(1),
        ..Default::default()
    });
    let identity = budgeted.run_identity(&PipelineData).unwrap();
    assert!(identity.as_deref().unwrap().contains("workflow-budget-v1"));
    assert_ne!(identity, budgeted.resume_identity);
}

#[tokio::test]
async fn expired_budget_journals_each_skipped_rollback_and_still_cancels() {
    use flare_workflow::{
        EntryResult, FunctionStep, JournalEntry, StepDefinition, StepId, StepResult, StepStatus,
        WorkflowContext, WorkflowDefinition,
    };
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let run = seed(
        &store,
        BudgetLimits {
            max_elapsed_secs: Some(1),
            ..Default::default()
        },
    )
    .await;
    store
        .update(run, |state| {
            state.status = WorkflowStatus::Paused;
            state.budget.as_mut().unwrap().started_at = Utc::now() - chrono::Duration::seconds(2);
            for id in ["a", "b"] {
                state.step_states.insert(
                    StepId::new(id),
                    StepState {
                        status: StepStatus::Succeeded,
                        ..Default::default()
                    },
                );
            }
        })
        .await
        .unwrap();
    let context = store.load(run).await.unwrap().context;
    let calls = Arc::new(AtomicUsize::new(0));
    let mut definition = WorkflowDefinition::new("test", "test");
    for id in ["a", "b"] {
        store
            .append_journal(
                run,
                JournalEntry::StepRun {
                    step_id: StepId::new(id),
                    attempt: 1,
                    result: Some(EntryResult::Success(serde_json::to_vec(&context).unwrap())),
                },
            )
            .await
            .unwrap();
        let calls = calls.clone();
        definition = definition.add_step(
            StepDefinition::new(id, id, flare_workflow::executor::noop_executor()).with_rollback(
                Arc::new(FunctionStep::new(
                    move |_: &mut WorkflowContext<PipelineData>| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async { Ok(StepResult::Success) })
                    },
                )),
                None,
            ),
        );
    }
    let engine = WorkflowEngine::with_store(store.clone());
    engine.register_workflow(definition).unwrap();
    engine.cancel_workflow_with_rollback(run).await.unwrap();
    assert_eq!(
        store.load(run).await.unwrap().status,
        WorkflowStatus::Cancelled
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let journal = store.journal(run).await.unwrap();
    for id in ["a", "b"] {
        assert!(journal.iter().any(|entry| matches!(entry,
            JournalEntry::Rollback { step_id, result: Some(EntryResult::Failure { message, .. }), .. }
                if step_id == &StepId::new(id) && message.contains("elapsed allowance exhausted")
        )), "missing rollback failure for {id}");
    }
}
