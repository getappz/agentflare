use super::{sdd_test_support::*, *};

#[tokio::test]
async fn sixth_fix_round_fails_the_step() {
    let send: flare_workflow::json::SendMessage = std::sync::Arc::new(
        move |inv: flare_workflow::json::StepInvocation| {
            let p = inv.prompt;
            Box::pin(async move {
                let r = if p.contains("judge") {
                    r#"{"action":"fix_round","rationale":"x","ledger_line":"x","task_model_tier":null}"#
                } else {
                    "REVIEW_ISSUES: x"
                };
                Ok((r.to_string(), 5u64, 5u64))
            })
        },
    );
    let mut d = one_task_data();
    d.fix_round = MAX_FIX_ROUNDS;
    d.review_issues = Some("x".to_string());
    d.last_report = Some("x".to_string());
    let step = build_sdd_loop_step(send);
    let mut ctx = WorkflowContext::new(Default::default(), d);
    match step.executor.execute(&mut ctx).await.expect("x") {
        StepResult::Failed(message) => {
            assert!(
                message.contains("MAX_FIX_ROUNDS"),
                "expected the real cap-exceeded reason, got: {message}"
            );
        }
        other => panic!("expected StepResult::Failed with a reason, got: {other:?}"),
    }
}
#[tokio::test]
async fn max_tasks_processed_bound_fails_the_step() {
    let (send, _) = mock_send(vec![]);
    let mut d = one_task_data();
    d.current_task_index = MAX_TASKS_PROCESSED;
    let step = build_sdd_loop_step(send);
    let mut ctx = WorkflowContext::new(Default::default(), d);
    match step.executor.execute(&mut ctx).await.expect("x") {
        StepResult::Failed(message) => {
            assert!(
                message.contains("MAX_TASKS_PROCESSED"),
                "expected the real cap-exceeded reason, got: {message}"
            );
        }
        other => panic!("expected StepResult::Failed with a reason, got: {other:?}"),
    }
}
