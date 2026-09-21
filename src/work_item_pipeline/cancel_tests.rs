//! Item #607: a job cancelled by a reassignment must stop the pipeline
//! without retries, and must never reach `finalize`'s push/PR.
use super::{sdd_test_support::*, *};

/// Registers a cancelled job the way `WorkerPool` does for a running one and
/// returns the claim owner that job works under.
fn cancelled_job_owner(job_id: &str) -> (agentflare_jobs::cancel::Registration, String) {
    (
        agentflare_jobs::cancel::register(job_id, || true),
        format!("opencode:{job_id}"),
    )
}

#[tokio::test]
async fn send_hook_refuses_another_agent_turn_for_a_cancelled_job() {
    let (_registered, owner) = cancelled_job_owner("hook-cancelled-job");
    let send = real_agent_send_hook(
        std::time::Duration::from_secs(1),
        std::time::Duration::from_secs(1),
        Vec::new(),
    );
    let inv = flare_workflow::json::StepInvocation {
        owner: Some(owner),
        ..flare_workflow::json::StepInvocation::simple("opencode".to_string(), "work".to_string())
    };

    let err = send(inv).await.expect_err("no agent may be started");

    assert_eq!(err, agentflare_jobs::cancel::CANCELLED_MESSAGE);
}

#[tokio::test]
async fn sdd_loop_fails_without_retry_when_its_agent_turn_is_cancelled() {
    let send: flare_workflow::json::SendMessage = std::sync::Arc::new(|_| {
        Box::pin(async { Err(agentflare_jobs::cancel::CANCELLED_MESSAGE.to_string()) })
    });
    let step = build_sdd_loop_step(send);
    let mut ctx = WorkflowContext::new(Default::default(), one_task_data());

    // `Ok(StepResult::Failed)`, not `Err`: an `Err` would burn the step's
    // retry policy re-asking a job that is already cancelled.
    match step.executor.execute(&mut ctx).await.expect("not an Err") {
        StepResult::Failed(message) => {
            assert_eq!(message, agentflare_jobs::cancel::CANCELLED_MESSAGE)
        }
        other => panic!("expected StepResult::Failed, got: {other:?}"),
    }
}

#[tokio::test]
async fn finalize_does_not_push_or_open_a_pr_for_a_cancelled_job() {
    let (_registered, owner) = cancelled_job_owner("finalize-cancelled-job");
    let data = WorkItemData {
        item_id: "item-1".into(),
        owner,
        reply_text: "implemented the thing".into(),
        ..Default::default()
    };
    // A default `AgentflareMcp` would open the real backend if finalize got
    // that far; the cancel check must return first.
    let step = build_finalize_step(std::sync::Arc::new(AgentflareMcp::default()));
    let mut ctx = WorkflowContext::new(Default::default(), data);

    match step.executor.execute(&mut ctx).await.expect("not an Err") {
        StepResult::Failed(message) => {
            assert_eq!(message, agentflare_jobs::cancel::CANCELLED_MESSAGE)
        }
        other => panic!("expected StepResult::Failed, got: {other:?}"),
    }
}
