//! Builds and runs the per-work-item `flare-workflow` pipeline: `coder` →
//! a bounded `review_or_fix` loop → `finalize`. See
//! `docs` item #110 for the design; corrected against the crate's real
//! Rust builder API (not the JSON/OpenFang schema — `finalize` runs real
//! Rust logic, not an agent prompt).

/// Cap on review/fix cycles before an item is gated for a human instead of
/// looping forever on an agent that can't converge. Mirrors
/// `quota::decide::SELF_REPAIR_CAP`'s existing cap-constant pattern.
pub(crate) const MAX_REVIEW_CYCLES: u32 = 3;

/// `flare_workflow::WorkflowId` name for this pipeline definition —
/// registered once at daemon boot (see `src/dashboard/server.rs`) and
/// referenced by every dispatched item's run.
pub(crate) const WORKFLOW_ID: &str = "agentflare-work-item";

/// Per-run state threaded through `coder` → `review_or_fix` → `finalize`.
/// `flare_workflow::WorkflowContext::data` persists and mutates across
/// steps within a run — this is where step results live, not the
/// `input`/`output` string channel (which only carries the loop's own
/// phase signal, see `build_review_or_fix_step`).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct WorkItemData {
    pub reply_text: String,
    pub session_id: Option<String>,
    pub cost_usd: Option<f64>,
    /// Set when `coder` detects an `AGENTFLARE_HOLD:` signal — short-circuits
    /// the rest of the pipeline straight to `item_release` (see Task 4).
    pub hold_reason: Option<String>,
    /// Latest unresolved reviewer findings, if any — read by `finalize`'s
    /// cap-exceeded path to post a useful gate comment.
    pub review_issues: Option<String>,
    pub pr_url: Option<String>,
}

impl flare_workflow::WorkflowData for WorkItemData {
    fn workflow_type() -> &'static str {
        WORKFLOW_ID
    }
}

use flare_workflow::executor::FunctionStep;
use flare_workflow::{StepDefinition, StepId, StepResult, WorkflowContext, WorkflowError};

/// Grep a headless reply for `AGENTFLARE_HOLD: <reason>`, same convention
/// `cli::work::detect_hold_signal` already uses — duplicated rather than
/// imported because `cli::work`'s version is a private `fn` and this crate
/// module is lower-level than `cli`; keep them in sync by hand if either
/// changes (both grep the same literal prefix agents are told to use).
fn detect_hold_signal(reply: &str) -> Option<&str> {
    reply.lines().find_map(|line| {
        let reason = line.trim().strip_prefix("AGENTFLARE_HOLD:")?.trim();
        (!reason.is_empty()).then_some(reason)
    })
}

/// Real entry point: dispatch to `crate::workflow::agent_send_hook()`.
pub(crate) fn build_coder_step(
    agent: agent_registry::Agent,
    prompt: String,
) -> StepDefinition<WorkItemData> {
    build_coder_step_with_sender(
        agent.as_str().to_string(),
        prompt,
        crate::workflow::agent_send_hook(),
    )
}

/// Test seam: same step, an injected `SendMessage` instead of the real
/// headless agent hook (mirrors `src/workflow.rs`'s own
/// `run_workflow_json_with_sender` test seam).
fn build_coder_step_with_sender(
    agent_name: String,
    prompt: String,
    send: flare_workflow::json::SendMessage,
) -> StepDefinition<WorkItemData> {
    StepDefinition::new(
        "coder",
        "coder",
        std::sync::Arc::new(FunctionStep::new(
            move |ctx: &mut WorkflowContext<WorkItemData>| {
                let send = send.clone();
                let agent_name = agent_name.clone();
                let prompt = prompt.clone();
                Box::pin(async move {
                    let (reply, in_tok, out_tok) =
                        send(agent_name, prompt)
                            .await
                            .map_err(|message| WorkflowError::StepFailed {
                                step_id: StepId::new("coder"),
                                message,
                            })?;
                    ctx.input_tokens += in_tok;
                    ctx.output_tokens += out_tok;
                    if let Some(reason) = detect_hold_signal(&reply) {
                        ctx.data.hold_reason = Some(reason.to_string());
                    } else {
                        ctx.data.reply_text = reply;
                    }
                    ctx.output = ctx.data.reply_text.clone();
                    Ok(StepResult::Success)
                })
            },
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_item_data_round_trips_through_json() {
        let data = WorkItemData {
            reply_text: "did the thing".into(),
            session_id: Some("sess-1".into()),
            cost_usd: Some(0.42),
            hold_reason: None,
            review_issues: Some("- fix the thing".into()),
            pr_url: Some("https://github.com/x/y/pull/1".into()),
        };
        let json = serde_json::to_string(&data).unwrap();
        let back: WorkItemData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reply_text, "did the thing");
        assert_eq!(back.pr_url.as_deref(), Some("https://github.com/x/y/pull/1"));
    }

    use flare_workflow::store::InMemoryStore;
    use flare_workflow::{WorkflowDefinition, WorkflowEngine, WorkflowId};
    use std::sync::Arc;

    fn mock_send_ok(reply: &'static str) -> flare_workflow::json::SendMessage {
        Arc::new(move |_agent: String, _prompt: String| {
            Box::pin(async move { Ok((reply.to_string(), 10u64, 0u64)) })
        })
    }

    #[tokio::test]
    async fn coder_step_populates_reply_text_and_no_hold_reason() {
        let step = build_coder_step_with_sender(
            "agent".to_string(),
            "Work item #1 — do the thing\n".to_string(),
            mock_send_ok("implemented it"),
        );
        let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);

        let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
        engine.register_workflow(wf).unwrap();
        let run_id = engine
            .start_workflow(WorkflowId::new(WORKFLOW_ID), WorkItemData::default(), String::new())
            .await
            .unwrap();

        for _ in 0..50 {
            let state = engine.get_status(run_id).await.unwrap();
            if state.status == flare_workflow::WorkflowStatus::Completed {
                assert_eq!(state.context.data.reply_text, "implemented it");
                assert!(state.context.data.hold_reason.is_none());
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("coder step did not complete");
    }

    #[tokio::test]
    async fn coder_step_sets_hold_reason_and_leaves_reply_text_empty() {
        let step = build_coder_step_with_sender(
            "agent".to_string(),
            "prompt".to_string(),
            mock_send_ok("looked into it\nAGENTFLARE_HOLD: waiting on PR #1"),
        );
        let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
        let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
        engine.register_workflow(wf).unwrap();
        let run_id = engine
            .start_workflow(WorkflowId::new(WORKFLOW_ID), WorkItemData::default(), String::new())
            .await
            .unwrap();

        for _ in 0..50 {
            let state = engine.get_status(run_id).await.unwrap();
            if state.status == flare_workflow::WorkflowStatus::Completed {
                assert_eq!(
                    state.context.data.hold_reason.as_deref(),
                    Some("waiting on PR #1")
                );
                assert!(state.context.data.reply_text.is_empty());
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("coder step did not complete");
    }
}
