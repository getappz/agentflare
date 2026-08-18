use super::*;

#[test]
fn sdd_pipeline_has_two_steps_with_correct_dependency() {
    let send: flare_workflow::json::SendMessage =
        std::sync::Arc::new(|_: flare_workflow::json::StepInvocation| {
            Box::pin(async { Ok((String::new(), 0, 0)) })
        });
    let pipeline = build_work_item_pipeline_with_sender(
        agent_registry::Agent::ClaudeCode,
        agent_registry::Agent::ClaudeCode,
        "Fix the null pointer in parser.rs".to_string(),
        None,
        std::sync::Arc::new(AgentflareMcp::default()),
        "item-1".to_string(),
        "opencode:test".to_string(),
        None,
        send,
    );
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
    let pipeline = build_work_item_pipeline_with_sender(
        agent_registry::Agent::Opencode,
        agent_registry::Agent::ClaudeCode,
        "Fix the null pointer in parser.rs".to_string(),
        None,
        std::sync::Arc::new(AgentflareMcp::default()),
        "item-1".to_string(),
        "opencode:test".to_string(),
        None,
        send,
    );
    let mut ctx =
        WorkflowContext::new(Default::default(), super::sdd_test_support::one_task_data());
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
    let step = build_sdd_loop_step("agent".to_string(), "agent".to_string(), send);
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
