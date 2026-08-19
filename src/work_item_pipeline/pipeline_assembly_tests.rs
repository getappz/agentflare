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
    assert_eq!(args, vec!["--resume".to_string(), "sess-1".to_string()]);
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
