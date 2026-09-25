use super::*;

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
        "did the fix",
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
        Some(&"sess-1".to_string())
    );
    assert_eq!(ctx.data.session_id.as_deref(), Some("sess-1"));

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
