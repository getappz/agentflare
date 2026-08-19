use super::sdd_test_support::*;
use super::*;

#[tokio::test]
async fn first_iteration_dispatches_implementer_then_judge() {
    let (send, calls) = mock_send(vec![
        "DONE: added the flag",
        r#"{"action":"advance_task","rationale":"looks done","ledger_line":"Task 0: implementer done","task_model_tier":null}"#,
    ]);
    let step = sdd_step(send);
    let mut ctx = WorkflowContext::new(Default::default(), one_task_data());
    let result = step.executor.execute(&mut ctx).await.expect("executes");
    assert!(matches!(result, StepResult::Success));

    let recorded = calls.lock().unwrap();
    assert_eq!(recorded[0].0, "implementer-agent");
    assert_eq!(recorded[1].0, "judge-agent");
    assert_eq!(
        ctx.data.ledger,
        vec!["Task 0: implementer done".to_string()]
    );
}

#[tokio::test]
async fn review_only_task_dispatches_review_analyst_prompt_not_implementer() {
    let (send, calls) = mock_send(vec![
        "Findings: none, changes look correct",
        r#"{"action":"advance_task","rationale":"analysis complete","ledger_line":"Task 0: reviewed","task_model_tier":null}"#,
    ]);
    let mut data = one_task_data();
    data.review_only = true;
    let step = sdd_step(send);
    let mut ctx = WorkflowContext::new(Default::default(), data);
    step.executor.execute(&mut ctx).await.expect("executes");

    let recorded = calls.lock().unwrap();
    assert!(
        recorded[0].1.contains("analysis only"),
        "review-only task must dispatch the review-analyst prompt, got: {}",
        recorded[0].1
    );
    assert!(
        !recorded[0].1.contains("You are implementing one task"),
        "review-only task must not dispatch the implementer prompt"
    );
    assert!(
        recorded[1].1.contains("review-only task"),
        "judge must be told this is a review-only task"
    );
}

#[tokio::test]
async fn task_reviewer_dispatches_on_the_judge_agent_not_the_implementer_agent() {
    let (send, calls) = mock_send(vec![
        "REVIEW_APPROVED",
        r#"{"action":"complete_pipeline","rationale":"done","ledger_line":"Task 0: complete","task_model_tier":null}"#,
    ]);
    let mut data = one_task_data();
    // A non-empty `last_report` with no open `review_issues` routes this
    // iteration to the task-reviewer, not the implementer.
    data.last_report = Some("DONE: added the flag".to_string());
    let step = sdd_step(send);
    let mut ctx = WorkflowContext::new(Default::default(), data);
    step.executor.execute(&mut ctx).await.expect("executes");

    let recorded = calls.lock().unwrap();
    assert_eq!(
        recorded[0].0, "judge-agent",
        "task-reviewer must dispatch on the reserved judge/review agent, not the implementer agent"
    );
}

#[tokio::test]
async fn re_reviewer_dispatches_on_the_judge_agent_not_the_implementer_agent() {
    let (send, calls) = mock_send(vec![
        "REVIEW_ISSUES: missing null check on line 12",
        r#"{"action":"fix_round","rationale":"issues found","ledger_line":"Task 0: fix round 1","task_model_tier":null}"#,
        "DONE: added the null check",
        r#"{"action":"continue_task","rationale":"awaiting re-review","ledger_line":"Task 0: fix submitted","task_model_tier":null}"#,
        "REVIEW_APPROVED",
        r#"{"action":"advance_task","rationale":"fix verified","ledger_line":"Task 0: complete","task_model_tier":null}"#,
    ]);
    let mut data = one_task_data();
    data.last_report = Some("DONE: initial attempt".to_string());
    let step = sdd_step(send);
    let mut ctx = WorkflowContext::new(Default::default(), data);

    step.executor.execute(&mut ctx).await.expect("round 1"); // task-reviewer -> fix_round
    step.executor.execute(&mut ctx).await.expect("round 2"); // implementer -> continue_task
    step.executor.execute(&mut ctx).await.expect("round 3"); // re-reviewer

    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 6);
    assert_eq!(
        recorded[4].0, "judge-agent",
        "re-reviewer must dispatch on the reserved judge/review agent, not the implementer agent"
    );
}

#[tokio::test]
async fn complete_pipeline_action_sets_terminator_output() {
    let (send, _calls) = mock_send(vec![
        "REVIEW_APPROVED",
        r#"{"action":"complete_pipeline","rationale":"all done","ledger_line":"Pipeline: complete","task_model_tier":null}"#,
    ]);
    let mut data = one_task_data();
    // A non-empty `last_report` with no open `review_issues`/fix round
    // routes this iteration to the task-reviewer, not the implementer.
    data.last_report = Some("DONE: added the flag".to_string());
    let step = sdd_step(send);
    let mut ctx = WorkflowContext::new(Default::default(), data);
    step.executor.execute(&mut ctx).await.expect("executes");
    assert_eq!(ctx.output, "PIPELINE_COMPLETE");
}

#[tokio::test]
async fn fix_round_dispatches_implementer_not_re_reviewer_next_iteration() {
    // Round 1: the reviewer finds issues and the judge issues a
    // `fix_round` decision, bumping `fix_round` to 1 in this SAME
    // iteration, before the implementer ever runs. Round 2 must NOT read
    // `fix_round > 0` as "a fix was already submitted" — it must
    // dispatch the implementer, not re-review a stale report.
    let (send, calls) = mock_send(vec![
        "REVIEW_ISSUES: missing null check on line 12",
        r#"{"action":"fix_round","rationale":"issues found","ledger_line":"Task 0: fix round 1","task_model_tier":null}"#,
        "DONE: added the null check",
        r#"{"action":"continue_task","rationale":"awaiting re-review","ledger_line":"Task 0: fix submitted","task_model_tier":null}"#,
    ]);
    let mut data = one_task_data();
    data.last_report = Some("DONE: initial attempt".to_string());
    let step = sdd_step(send);
    let mut ctx = WorkflowContext::new(Default::default(), data);

    // Round 1: task-reviewer finds issues, judge calls fix_round.
    step.executor
        .execute(&mut ctx)
        .await
        .expect("round 1 executes");
    assert_eq!(ctx.data.fix_round, 1);
    assert_eq!(
        ctx.data.review_issues.as_deref(),
        Some("missing null check on line 12")
    );
    assert_eq!(
        ctx.data.last_report, None,
        "clearing last_report on REVIEW_ISSUES signals no fix attempt exists yet"
    );

    // Round 2: must dispatch the implementer with the findings as fix context.
    step.executor
        .execute(&mut ctx)
        .await
        .expect("round 2 executes");
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 4);
    let round2_role_prompt = &recorded[2].1;
    assert!(
        round2_role_prompt.contains("You are implementing one task"),
        "round 2 must dispatch the implementer, got prompt: {round2_role_prompt}"
    );
    assert!(
        round2_role_prompt.contains("missing null check on line 12"),
        "implementer prompt must carry the reviewer's findings as fix context"
    );
    assert!(
        !round2_role_prompt.contains("Re-review a fix"),
        "round 2 must NOT dispatch the re-reviewer"
    );
}

#[tokio::test]
async fn full_cycle_dispatches_re_reviewer_after_implementer_fix() {
    // Extends the above: reviewer finds issues -> fix_round -> implementer
    // fixes -> continue_task -> the FOLLOWING iteration must dispatch the
    // re-reviewer, proving the `last_report.is_some()` branch works too.
    let (send, calls) = mock_send(vec![
        "REVIEW_ISSUES: missing null check on line 12",
        r#"{"action":"fix_round","rationale":"issues found","ledger_line":"Task 0: fix round 1","task_model_tier":null}"#,
        "DONE: added the null check",
        r#"{"action":"continue_task","rationale":"awaiting re-review","ledger_line":"Task 0: fix submitted","task_model_tier":null}"#,
        "REVIEW_APPROVED",
        r#"{"action":"advance_task","rationale":"fix verified","ledger_line":"Task 0: complete","task_model_tier":null}"#,
    ]);
    let mut data = one_task_data();
    data.last_report = Some("DONE: initial attempt".to_string());
    let step = sdd_step(send);
    let mut ctx = WorkflowContext::new(Default::default(), data);

    step.executor
        .execute(&mut ctx)
        .await
        .expect("round 1 executes"); // task-reviewer -> fix_round
    step.executor
        .execute(&mut ctx)
        .await
        .expect("round 2 executes"); // implementer -> continue_task
    assert_eq!(
        ctx.data.last_report.as_deref(),
        Some("DONE: added the null check")
    );

    step.executor
        .execute(&mut ctx)
        .await
        .expect("round 3 executes"); // must be re-reviewer
    let recorded = calls.lock().unwrap();
    assert_eq!(recorded.len(), 6);
    let round3_role_prompt = &recorded[4].1;
    assert!(
        round3_role_prompt.contains("Re-review a fix for this task's findings only"),
        "round 3 must dispatch the re-reviewer, got prompt: {round3_role_prompt}"
    );
    assert!(
        round3_role_prompt.contains("missing null check on line 12"),
        "re-reviewer prompt must carry the original findings"
    );
    assert!(
        round3_role_prompt.contains("DONE: added the null check"),
        "re-reviewer prompt must carry the fix report"
    );
}

#[tokio::test]
async fn judge_parse_failure_is_retryable_step_error() {
    // Must be a real `Err`, not `Ok(StepResult::Failure)` — the engine's
    // `execute_step_with_retry` (`crates/flare-workflow/src/engine.rs`)
    // only ever consults the step's `RetryPolicy` for a genuine `Err`;
    // `Ok(StepResult::Failure)` is hardcoded non-retryable regardless of
    // policy. A malformed judge reply is exactly the transient case
    // `sdd_loop`'s attached `RetryPolicy` (3 attempts) exists for.
    let (send, _calls) = mock_send(vec!["DONE: added the flag", "not json"]);
    let step = sdd_step(send);
    let mut ctx = WorkflowContext::new(Default::default(), one_task_data());
    let err = step
        .executor
        .execute(&mut ctx)
        .await
        .expect_err("malformed judge reply must surface as Err to be retried");
    assert!(matches!(err, WorkflowError::StepFailed { .. }));
}

#[tokio::test]
async fn resumed_iteration_dispatches_next_task_not_a_repeat() {
    // Simulates the crash-resume mechanism directly at the ctx.data level
    // (per the spec's corrected Resumability section: the engine's own
    // loop iteration counter is NOT durable across a crash — only ctx.data
    // is, via state_store.update() after each completed iteration). This
    // test proves the closure's own behavior is correct given
    // already-advanced ctx.data, which is the actual resumability
    // guarantee — it does not exercise the engine's crash/restart
    // machinery itself (that's flare-workflow's own test suite's job).
    let (send, calls) = mock_send(vec![
        "DONE: task 2 implemented",
        r#"{"action":"advance_task","rationale":"done","ledger_line":"Task 1: complete","task_model_tier":null}"#,
    ]);
    let step = sdd_step(send);

    // ctx.data as it would look immediately after a crash that happened
    // right after task 0's advance_task was applied and persisted.
    let data = WorkItemData {
        agent_name: "implementer-agent".to_string(),
        judge_agent_name: "judge-agent".to_string(),
        tasks: vec![
            SddTask {
                id: 0,
                title: "Task 1".to_string(),
                body: "first".to_string(),
                model_tier: None,
            },
            SddTask {
                id: 1,
                title: "Task 2".to_string(),
                body: "second".to_string(),
                model_tier: None,
            },
        ],
        current_task_index: 1, // already advanced past task 0
        ledger: vec!["Task 0: complete".to_string()],
        ..Default::default()
    };
    let mut ctx = WorkflowContext::new(Default::default(), data);

    step.executor.execute(&mut ctx).await.expect("executes");

    let recorded = calls.lock().unwrap();
    assert!(
        recorded[0].1.contains("second"),
        "must dispatch task 1's (index 1) body, not task 0's"
    );
    assert!(
        !recorded[0].1.contains("first"),
        "must not re-dispatch the already-completed task"
    );
}

#[test]
fn single_task_synthesized_from_item_description_when_no_plan_doc() {
    let tasks = load_or_synthesize_tasks("Fix the off-by-one in pagination", None);
    assert_eq!(tasks.len(), 1);
    // Degenerate case: exactly #110's original shape — one implementer
    // dispatch, one review, no fix-loop-specific task list machinery
    // engaged beyond what a single task naturally exercises.
    assert_eq!(tasks[0].body, "Fix the off-by-one in pagination");
}

#[tokio::test]
async fn single_task_plan_reaches_complete_pipeline_after_one_approved_review() {
    let (send, _calls) = mock_send(vec![
        "DONE: fixed pagination",
        r#"{"action":"advance_task","rationale":"impl done, needs review next","ledger_line":"Task 0: implemented","task_model_tier":null}"#,
    ]);
    let step = sdd_step(send);
    let tasks = load_or_synthesize_tasks("Fix the off-by-one in pagination", None);
    let data = WorkItemData {
        agent_name: "implementer-agent".to_string(),
        judge_agent_name: "judge-agent".to_string(),
        tasks,
        ..Default::default()
    };
    let mut ctx = WorkflowContext::new(Default::default(), data);
    step.executor.execute(&mut ctx).await.expect("executes");
    assert_eq!(
        ctx.data.current_task_index, 1,
        "advanced past the only task"
    );
    assert_eq!(
        ctx.output, "CONTINUE",
        "next iteration will see current_task_index >= tasks.len() and complete"
    );
}

#[tokio::test]
async fn three_task_plan_with_fix_round_escalation_and_skip() {
    // Task 0: implementer -> reviewer finds issues -> fix round -> re-review approves -> advance.
    // Task 1: judge decides to skip outright.
    // Task 2: implementer -> reviewer approves -> advance -> judge completes pipeline.
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    let responses = VecDeque::from(vec![
        // Task 0, iteration 1: implementer
        "DONE: task 0 attempt 1",
        r#"{"action":"continue_task","rationale":"needs review","ledger_line":"Task 0: implementer done","task_model_tier":"mechanical"}"#,
        // iteration 2: task-reviewer finds issues
        "REVIEW_ISSUES: missing edge case",
        r#"{"action":"fix_round","rationale":"real finding","ledger_line":"Task 0: fix round 1/5","task_model_tier":null}"#,
        // iteration 3: implementer fixes
        "DONE: fixed edge case",
        r#"{"action":"continue_task","rationale":"needs re-review","ledger_line":"Task 0: fix applied","task_model_tier":null}"#,
        // iteration 4: re-reviewer approves
        "REVIEW_APPROVED",
        r#"{"action":"advance_task","rationale":"clean","ledger_line":"Task 0: complete","task_model_tier":null}"#,
        // Task 1, iteration 5: judge skips outright after seeing the role reply
        "DONE: task 1 attempted",
        r#"{"action":"skip_task","rationale":"superseded by task 0's fix","ledger_line":"Task 1: skipped","task_model_tier":null}"#,
        // Task 2, iteration 6: implementer
        "DONE: task 2 implemented",
        r#"{"action":"continue_task","rationale":"needs review","ledger_line":"Task 2: implementer done","task_model_tier":null}"#,
        // iteration 7: reviewer approves
        "REVIEW_APPROVED",
        r#"{"action":"advance_task","rationale":"clean","ledger_line":"Task 2: complete","task_model_tier":null}"#,
    ]);
    let responses = Arc::new(Mutex::new(responses));
    let send: flare_workflow::json::SendMessage =
        Arc::new(move |_: flare_workflow::json::StepInvocation| {
            let reply = responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default()
                .to_string();
            Box::pin(async move { Ok((reply, 5u64, 5u64)) })
        });

    let step = sdd_step(send);
    let data = WorkItemData {
        agent_name: "implementer-agent".to_string(),
        judge_agent_name: "judge-agent".to_string(),
        tasks: vec![
            SddTask {
                id: 0,
                title: "Task 0".to_string(),
                body: "first".to_string(),
                model_tier: None,
            },
            SddTask {
                id: 1,
                title: "Task 1".to_string(),
                body: "second".to_string(),
                model_tier: None,
            },
            SddTask {
                id: 2,
                title: "Task 2".to_string(),
                body: "third".to_string(),
                model_tier: None,
            },
        ],
        ..Default::default()
    };
    let mut ctx = WorkflowContext::new(Default::default(), data);

    // Drive iterations manually until PIPELINE_COMPLETE or a safety cap —
    // this test exercises SddLoopExecutor::execute directly in a loop,
    // mirroring what the engine's execute_loop would do, without needing
    // the full WorkflowEngine/state store machinery.
    for _ in 0..20 {
        let result = step.executor.execute(&mut ctx).await.expect("executes");
        assert!(matches!(result, flare_workflow::StepResult::Success));
        if ctx.output == "PIPELINE_COMPLETE" {
            break;
        }
    }

    assert_eq!(ctx.output, "PIPELINE_COMPLETE");
    assert!(ctx.data.ledger.iter().any(|l| l.contains("fix round 1/5")));
    assert!(ctx.data.ledger.iter().any(|l| l.contains("skipped")));
    assert_eq!(
        ctx.data
            .ledger
            .iter()
            .filter(|l| l.contains("complete"))
            .count(),
        2,
        "task 0 and task 2 both completed"
    );
}

/// End-to-end proof of the crash-resume fix: drives a real `WorkflowEngine`
/// + `SqliteStore` across two "processes" sharing one on-disk DB file,
/// mirroring `flare-workflow/tests/recovery_test.rs`'s own
/// `crash_mid_run_resumes_from_last_completed_step_without_reexecution`.
/// `sdd_loop` has no built-in park point (unlike `WaitEvent`/`Sleep`), so the
/// crash is simulated the same way that test simulates one: the mock `send`
/// for task 1's role dispatch never resolves, standing in for the process
/// dying mid-await; the first engine is then dropped without ever
/// completing task 1.
#[tokio::test]
async fn crash_mid_sdd_loop_resumes_against_the_persisted_item_and_agent() {
    use flare_workflow::{SqliteStore, StateStore, WorkflowEngine, WorkflowId};
    use std::sync::{Arc, Mutex};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wf.db");

    let tasks = vec![
        SddTask {
            id: 0,
            title: "Task 0".to_string(),
            body: "first task body".to_string(),
            model_tier: None,
        },
        SddTask {
            id: 1,
            title: "Task 1".to_string(),
            body: "second task body".to_string(),
            model_tier: None,
        },
    ];
    let seed_data = WorkItemData {
        item_id: "item-crash-test".to_string(),
        agent_name: "impl-agent".to_string(),
        judge_agent_name: "judge-agent".to_string(),
        tasks: tasks.clone(),
        ..Default::default()
    };

    let run_id;
    {
        // "Process 1": completes task 0, then hangs forever dispatching
        // task 1's implementer instead of ever completing it.
        let send: flare_workflow::json::SendMessage = Arc::new(
            move |inv: flare_workflow::json::StepInvocation| {
                let prompt = inv.prompt;
                Box::pin(async move {
                    if prompt.contains("second task body") {
                        std::future::pending::<()>().await;
                        unreachable!("crash point: never resolves")
                    } else if prompt.contains("You are the judge") {
                        Ok((
                            r#"{"action":"advance_task","rationale":"done","ledger_line":"Task 0: complete","task_model_tier":null}"#
                                .to_string(),
                            1u64,
                            1u64,
                        ))
                    } else {
                        Ok(("DONE: task 0 implemented".to_string(), 1u64, 1u64))
                    }
                })
            },
        );
        let engine = WorkflowEngine::<WorkItemData, SqliteStore<WorkItemData>>::with_store(
            SqliteStore::open_file(&path).unwrap(),
        );
        engine
            .register_workflow(
                flare_workflow::WorkflowDefinition::new(WORKFLOW_ID, "sdd work item")
                    .add_step(sdd_step(send)),
            )
            .unwrap();
        run_id = engine
            .start_workflow(WorkflowId::new(WORKFLOW_ID), seed_data, String::new())
            .await
            .unwrap();

        let store = engine.state_store().clone();
        for _ in 0..200 {
            let journal = store.journal(run_id).await.unwrap();
            let task0_done = journal.iter().any(|e| {
                matches!(
                    e,
                    flare_workflow::JournalEntry::LoopIteration { step_id, iteration, .. }
                        if *step_id == StepId::new("sdd_loop") && *iteration == 1
                )
            });
            if task0_done {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        // Drop engine 1 = simulated crash; the run is still `Running` in
        // the shared store, stuck mid task-1 dispatch.
    }

    // "Process 2": a fresh engine registered the same way the daemon's own
    // boot-time sweep does (`dashboard::server::run`) — no item/agent-
    // specific arguments anywhere in this registration, only a bare
    // `sdd_step`. Whatever runs next can only get task 1's real title/body
    // and the item's real agent name from the persisted `WorkItemData`,
    // proving recovery no longer replays against a registration-time
    // placeholder.
    let recorded: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded_clone = recorded.clone();
    let send2: flare_workflow::json::SendMessage = Arc::new(
        move |inv: flare_workflow::json::StepInvocation| {
            recorded_clone
                .lock()
                .unwrap()
                .push((inv.agent.clone(), inv.prompt.clone()));
            let prompt = inv.prompt;
            Box::pin(async move {
                if prompt.contains("You are the judge") {
                    Ok((
                        r#"{"action":"complete_pipeline","rationale":"done","ledger_line":"Task 1: complete","task_model_tier":null}"#
                            .to_string(),
                        1u64,
                        1u64,
                    ))
                } else {
                    Ok(("DONE: task 1 implemented".to_string(), 1u64, 1u64))
                }
            })
        },
    );
    let engine2 = WorkflowEngine::<WorkItemData, SqliteStore<WorkItemData>>::with_store(
        SqliteStore::open_file(&path).unwrap(),
    );
    engine2
        .register_workflow(
            flare_workflow::WorkflowDefinition::new(WORKFLOW_ID, "sdd work item")
                .add_step(sdd_step(send2)),
        )
        .unwrap();
    let resumed = engine2.recover().await.unwrap();
    assert!(
        resumed.contains(&run_id),
        "recover() must resume the crashed run"
    );

    let mut state = engine2.get_status(run_id).await.unwrap();
    for _ in 0..200 {
        if state.status == flare_workflow::WorkflowStatus::Completed {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        state = engine2.get_status(run_id).await.unwrap();
    }

    assert_eq!(state.status, flare_workflow::WorkflowStatus::Completed);
    let recorded = recorded.lock().unwrap();
    assert!(
        recorded
            .iter()
            .any(|(agent, prompt)| agent == "impl-agent" && prompt.contains("second task body")),
        "resumed run must dispatch task 1's real body against the persisted implementer agent, \
         not a registration-time placeholder; recorded calls: {recorded:?}"
    );
}
