use super::*;

#[test]
fn budget_is_shared_by_the_role_and_judge_and_by_stale_session_fallback() {
    use flare_workflow::budget::{BudgetLimits, BudgetOperation};
    use flare_workflow::{
        StateStore, WorkflowDefinition, WorkflowEngine, WorkflowId, WorkflowStatus,
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    crate::paths::test_support::with_temp_home(|| {
        for stale in [false, true] {
            let calls = Arc::new(AtomicUsize::new(0));
            let count = calls.clone();
            let send: flare_workflow::SendMessage = Arc::new(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
                Box::pin(async move {
                    if stale {
                        Err("No conversation found with session ID stale".into())
                    } else {
                        Ok(("reviewed".into(), 0, 0))
                    }
                })
            });
            let store = flare_workflow::SqliteStore::<WorkItemData>::open_memory().unwrap();
            let engine = WorkflowEngine::with_store(store.clone());
            let mut data = super::sdd_test_support::one_task_data();
            data.review_only = true;
            data.workflow_budget = Some(BudgetLimits {
                max_invocations: Some(1),
                ..Default::default()
            });
            if stale {
                data.agent_name = "claude-code".into();
                data.agent_sessions
                    .insert("claude-code".into(), "stale".into());
            }
            let definition = WorkflowDefinition::new("budget-test", "budget-test")
                .add_step(build_sdd_loop_step(send));
            engine.register_workflow(definition).unwrap();
            crate::workflow::blocking_runtime().block_on(async {
                let run = engine
                    .start_workflow(WorkflowId::new("budget-test"), data, String::new())
                    .await
                    .unwrap();
                assert!(
                    engine
                        .wait_for_completion(run, "test", std::time::Duration::from_secs(5))
                        .await
                        .is_err()
                );
                assert_eq!(calls.load(Ordering::SeqCst), 1);
                let state = store.load(run).await.unwrap();
                assert_eq!(state.status, WorkflowStatus::Failed);
                assert!(
                    state
                        .error
                        .unwrap()
                        .contains("invocation allowance exhausted")
                );
                assert_eq!(
                    store
                        .budget_operation(run, BudgetOperation::Snapshot)
                        .await
                        .unwrap()
                        .state
                        .summary()["invocation_reservations"],
                    1
                );
            });
        }
    });
}

#[test]
fn budget_policy_changes_are_rejected_before_native_redispatch() {
    use flare_workflow::budget::{BudgetLimits, BudgetState};
    use flare_workflow::{StateStore, WorkflowId, WorkflowState, WorkflowStatus};
    crate::paths::test_support::with_temp_home(|| {
        let (mcp, _backend, _repo, item_id, _, path) =
            crate::mcp_server::tests::mcp_with_claimed_item("Budget resume policy fixture");
        let mcp = std::sync::Arc::new(mcp);
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
            .unwrap()
            .unwrap();
        let one = BudgetLimits {
            max_invocations: Some(1),
            ..Default::default()
        };
        let two = BudgetLimits {
            max_invocations: Some(2),
            ..Default::default()
        };
        for (old, requested, status, expected) in [
            (
                None,
                Some(one.clone()),
                Some(WorkflowStatus::Paused),
                "requested policy differs",
            ),
            (
                Some(two),
                Some(one.clone()),
                Some(WorkflowStatus::Paused),
                "requested policy differs",
            ),
            (
                Some(one.clone()),
                None,
                Some(WorkflowStatus::Paused),
                "requested policy differs",
            ),
            (
                Some(one.clone()),
                Some(one.clone()),
                Some(WorkflowStatus::Failed),
                "persisted budgeted run is terminal",
            ),
            (
                Some(one.clone()),
                Some(one.clone()),
                Some(WorkflowStatus::Cancelled),
                "persisted budgeted run is terminal",
            ),
            (
                Some(one.clone()),
                Some(one.clone()),
                None,
                "persisted run is unavailable",
            ),
        ] {
            let run = flare_workflow::WorkflowRunId::new();
            let mut data = super::sdd_test_support::one_task_data();
            data.item_id = item_id.clone();
            data.workflow_budget = old.clone();
            let mut state = WorkflowState::new(run, WorkflowId::new(WORKFLOW_ID), data);
            state.status = status.unwrap_or(WorkflowStatus::Paused);
            state.budget = old.map(|limits| BudgetState::new(limits, chrono::Utc::now()).unwrap());
            if status.is_some() {
                crate::workflow::blocking_runtime()
                    .block_on(engine().state_store().save(state))
                    .unwrap();
            }
            let mut item = item.clone();
            item.metadata =
                serde_json::json!({"workflow_run_id":run.to_string(),"workflow_budget":requested})
                    .to_string();
            let send: flare_workflow::SendMessage = std::sync::Arc::new(|_| {
                Box::pin(async { panic!("changed budget must not dispatch") })
            });
            let result = run_or_resume_with_sender(
                mcp.clone(),
                &item,
                &path,
                agent_registry::Agent::ClaudeCode,
                agent_registry::Agent::ClaudeCode,
                "inspect".into(),
                None,
                None,
                send,
            );
            assert!(result.unwrap_err().contains(expected));
        }
    });
}

#[test]
fn recovered_cursor_launch_restores_autonomy_and_streaming() {
    let role_args = vec!["--resume".to_string(), "session-379".to_string()];
    let args = work_item_launch_args("cursor", &[], &role_args);
    assert_eq!(
        args,
        [
            "--force",
            "--trust",
            "--approve-mcps",
            "--output-format",
            "stream-json",
            "--resume",
            "session-379"
        ]
    );
}

#[test]
fn work_item_launch_keeps_explicit_options_and_role_args() {
    let extra = vec!["--model".to_string(), "chosen-model".to_string()];
    let role = vec!["--resume".to_string(), "session-379".to_string()];
    let args = work_item_launch_args("cursor", &extra, &role);
    assert_eq!(args, ["--model", "chosen-model", "--resume", "session-379"]);
}

#[test]
fn sdd_pipeline_has_two_steps_with_correct_dependency() {
    let send: flare_workflow::json::SendMessage =
        std::sync::Arc::new(|_: flare_workflow::json::StepInvocation| {
            Box::pin(async { Ok((String::new(), 0, 0)) })
        });
    let pipeline =
        build_work_item_pipeline_with_sender(std::sync::Arc::new(AgentflareMcp::default()), send);
    assert_eq!(pipeline.steps.len(), 2);
    assert_eq!(pipeline.steps[0].id.to_string(), "sdd_loop");
    assert_eq!(pipeline.steps[1].id.to_string(), "finalize");
    assert_eq!(pipeline.steps[1].depends_on, vec![StepId::new("sdd_loop")]);
}

#[tokio::test]
async fn sdd_loop_dispatches_implementer_and_review_roles_on_their_own_agents() {
    let (send, calls) = super::sdd_test_support::mock_send(vec![
        "DONE: added the flag",
        r#"{"action":"advance_task","rationale":"looks done","ledger_line":"Task 0: implementer done","task_model_tier":null}"#,
    ]);
    let pipeline =
        build_work_item_pipeline_with_sender(std::sync::Arc::new(AgentflareMcp::default()), send);
    let mut data = super::sdd_test_support::one_task_data();
    data.agent_name = "opencode".to_string();
    data.judge_agent_name = "claude-code".to_string();
    let mut ctx = WorkflowContext::new(Default::default(), data);
    pipeline.steps[0]
        .executor
        .execute(&mut ctx)
        .await
        .expect("executes");

    let recorded = calls.lock().unwrap();
    assert_eq!(
        recorded[0].0, "opencode",
        "implementer role must dispatch on implementer_agent"
    );
    assert_eq!(
        recorded[1].0, "claude-code",
        "judge role must dispatch on review_agent"
    );
}

#[tokio::test]
async fn sdd_loop_resumes_the_implementer_session_on_the_next_fix_round() {
    let (send, calls) = super::sdd_test_support::mock_send(vec![
        // Iteration 1: implementer (claude-code) reports done, carrying a
        // session id back through the marker channel.
        "did the thing\u{0}AGENTFLARE_SESSION:sess-1",
        r#"{"action":"fix_round","rationale":"needs polish","ledger_line":"Task 0: fix round 1","task_model_tier":null}"#,
        // Iteration 2: task-reviewer (cursor) finds an issue.
        "REVIEW_ISSUES: needs a test",
        r#"{"action":"continue_task","rationale":"reviewing","ledger_line":"Task 0: review issues found","task_model_tier":null}"#,
        // Iteration 3: implementer (claude-code) fixes it -- this call
        // must resume iteration 1's session.
        "did the fix\u{0}AGENTFLARE_SESSION:sess-2",
        r#"{"action":"advance_task","rationale":"fixed","ledger_line":"Task 0: fixed","task_model_tier":null}"#,
    ]);
    let pipeline =
        build_work_item_pipeline_with_sender(std::sync::Arc::new(AgentflareMcp::default()), send);
    let mut data = super::sdd_test_support::one_task_data();
    data.agent_name = agent_registry::Agent::ClaudeCode.as_str().to_string();
    data.judge_agent_name = agent_registry::Agent::Cursor.as_str().to_string();
    let mut ctx = WorkflowContext::new(Default::default(), data);
    for _ in 0..3 {
        pipeline.steps[0]
            .executor
            .execute(&mut ctx)
            .await
            .expect("executes");
    }

    assert_eq!(
        ctx.data.agent_sessions.get("claude-code"),
        Some(&"sess-2".to_string())
    );
    assert_eq!(ctx.data.session_id.as_deref(), Some("sess-2"));
    assert_eq!(
        ctx.data.session_ids,
        ["sess-1".to_string(), "sess-2".to_string()].into()
    );

    let recorded = calls.lock().unwrap();
    let (agent, _prompt, args) = recorded[4].clone();
    assert_eq!(agent, "claude-code");
    assert_eq!(
        &args[..2],
        ["--resume".to_string(), "sess-1".to_string()],
        "the resume pair leads the argv"
    );
    assert!(
        args.contains(&"--append-system-prompt".to_string()),
        "the resumed round keeps its role identity as a flag: {args:?}"
    );
}

/// The value following `flag` in `args`, if present.
fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

#[tokio::test]
async fn sdd_loop_compiles_role_policy_into_claude_flags_and_folds_it_into_other_prompts() {
    let (send, calls) = super::sdd_test_support::mock_send(vec![
        "DONE: added the flag",
        r#"{"action":"advance_task","rationale":"looks done","ledger_line":"Task 0: implementer done","task_model_tier":null}"#,
    ]);
    let pipeline =
        build_work_item_pipeline_with_sender(std::sync::Arc::new(AgentflareMcp::default()), send);
    let mut data = super::sdd_test_support::one_task_data();
    data.agent_name = "opencode".to_string();
    data.judge_agent_name = "claude-code".to_string();
    let mut ctx = WorkflowContext::new(Default::default(), data);
    pipeline.steps[0]
        .executor
        .execute(&mut ctx)
        .await
        .expect("executes");

    let recorded = calls.lock().unwrap();

    // opencode has no confirmed system-prompt flag: the implementer's
    // identity is folded into the prompt and the argv stays empty.
    let (agent, prompt, args) = recorded[0].clone();
    assert_eq!(agent, "opencode");
    assert!(
        args.is_empty(),
        "no unconfirmed flags for opencode: {args:?}"
    );
    assert!(
        prompt.starts_with("You are the implementer in an agentflare SDD pipeline"),
        "identity folded into the prompt: {prompt:?}"
    );
    assert!(prompt.contains("Add --verbose"), "task body still present");

    // Claude Code carries the judge's identity and tool policy as flags,
    // so the prompt is not also prefixed with it.
    let (agent, prompt, args) = recorded[1].clone();
    assert_eq!(agent, "claude-code");
    assert_eq!(
        flag_value(&args, "--append-system-prompt").map(|s| s.starts_with("You are the judge")),
        Some(true),
        "{args:?}"
    );
    let denied = flag_value(&args, "--disallowedTools").expect("judge is tool-restricted");
    for tool in ["Edit", "Write", "NotebookEdit", "Bash"] {
        assert!(
            denied.split(',').any(|t| t == tool),
            "judge must not have {tool}: {denied}"
        );
    }
    assert!(
        !prompt.starts_with("You are the judge in an agentflare SDD pipeline"),
        "identity must not be duplicated into the prompt when the argv carries it"
    );
    assert!(prompt.contains("You are the judge for an autonomous multi-task execution pipeline"));
}

#[tokio::test]
async fn sdd_loop_reviewer_on_claude_code_is_read_only_but_keeps_the_shell() {
    let (send, calls) = super::sdd_test_support::mock_send(vec![
        // Iteration 1: implementer reports; judge asks to continue so the
        // report goes to review.
        "did the thing",
        r#"{"action":"continue_task","rationale":"review it","ledger_line":"Task 0: implemented","task_model_tier":null}"#,
        // Iteration 2: task reviewer approves; judge advances.
        "REVIEW_APPROVED",
        r#"{"action":"advance_task","rationale":"approved","ledger_line":"Task 0: approved","task_model_tier":null}"#,
    ]);
    let pipeline =
        build_work_item_pipeline_with_sender(std::sync::Arc::new(AgentflareMcp::default()), send);
    let mut data = super::sdd_test_support::one_task_data();
    data.agent_name = "claude-code".to_string();
    data.judge_agent_name = "claude-code".to_string();
    let mut ctx = WorkflowContext::new(Default::default(), data);
    for _ in 0..2 {
        pipeline.steps[0]
            .executor
            .execute(&mut ctx)
            .await
            .expect("executes");
    }

    let recorded = calls.lock().unwrap();
    let (_, _, implementer_args) = recorded[0].clone();
    assert!(
        flag_value(&implementer_args, "--disallowedTools").is_none(),
        "the implementer keeps every tool: {implementer_args:?}"
    );
    let (_, prompt, reviewer_args) = recorded[2].clone();
    assert!(
        prompt.contains("Review this task's implementation"),
        "{prompt:?}"
    );
    let denied = flag_value(&reviewer_args, "--disallowedTools").expect("reviewer is read-only");
    assert!(denied.split(',').any(|t| t == "Edit"), "{denied}");
    assert!(
        !denied.split(',').any(|t| t == "Bash"),
        "the reviewer must keep the shell to run verification: {denied}"
    );
}

/// Regression test: `sdd_loop`'s per-iteration engine timeout must not
/// fall back to `flare_workflow::WorkflowDefinition::new`'s 300s
/// library default -- a real implementer/reviewer/judge dispatch
/// routinely exceeds that, and `execute_loop` (`loops.rs`) kills the
/// whole iteration the instant it's hit ("Step timed out after 300s"),
/// which is exactly the failure every SDD-dispatched item hit before
/// this fix. It must instead line up with `supervisor::WORK_JOB_TIMEOUT_SECS`,
/// the outer job's own hard-cap budget this step runs inside.
#[test]
fn sdd_loop_timeout_matches_work_job_timeout_not_library_default() {
    let send: flare_workflow::json::SendMessage =
        std::sync::Arc::new(|_: flare_workflow::json::StepInvocation| {
            Box::pin(async { Ok((String::new(), 0, 0)) })
        });
    let step = build_sdd_loop_step(send);
    let configured_timeout = step.timeout.expect("sdd_loop must set an explicit timeout");
    assert_eq!(
        configured_timeout,
        std::time::Duration::from_secs(crate::supervisor::WORK_JOB_TIMEOUT_SECS)
    );
    assert_ne!(
        configured_timeout,
        std::time::Duration::from_secs(300),
        "sdd_loop must not fall back to the flare-workflow library's 300s default"
    );
}
