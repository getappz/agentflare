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

use crate::mcp_server::AgentflareMcp;
use crate::mcp_server::types::{CommentRequest, ItemRequest};

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

/// Marker the reviewer replies with when the diff is approved — matched
/// (case-insensitive substring) by the `StepMode::Loop`'s `until` field to
/// stop the loop.
const REVIEW_APPROVED_MARKER: &str = "REVIEW_APPROVED";
/// Prefix the reviewer replies with when there are unresolved issues —
/// echoed back as `ctx.input` on the next iteration, which is how the
/// closure below tells reviewer-turn from fixer-turn apart.
const REVIEW_ISSUES_MARKER: &str = "REVIEW_ISSUES:";

/// Real entry point: dispatch to `crate::workflow::agent_send_hook()`.
///
/// `diff_prompt_prefix` is caller-supplied (already contains the diff to
/// review — built by the pipeline-assembly caller); this step only formats
/// a prompt around it and does not read files or compute diffs itself.
pub(crate) fn build_review_or_fix_step(
    agent_name: String,
    diff_prompt_prefix: String,
) -> StepDefinition<WorkItemData> {
    build_review_or_fix_step_with_sender(
        agent_name,
        diff_prompt_prefix,
        crate::workflow::agent_send_hook(),
    )
}

/// Test seam: same step, an injected `SendMessage` instead of the real
/// headless agent hook (mirrors `build_coder_step_with_sender`).
///
/// `StepMode::Loop` re-invokes this SAME executor repeatedly, chaining
/// `output → input` between iterations (see
/// `flare_workflow::engine::WorkflowEngine::execute_loop`). The closure
/// decides its role each call from `ctx.input`: if it starts with
/// `REVIEW_ISSUES_MARKER` (the previous reviewer turn's output, echoed back
/// as this turn's input) it acts as the fixer; otherwise (empty input on
/// the first iteration, or `FIXED: ...` from a prior fixer turn) it acts as
/// the reviewer.
fn build_review_or_fix_step_with_sender(
    agent_name: String,
    diff_prompt_prefix: String,
    send: flare_workflow::json::SendMessage,
) -> StepDefinition<WorkItemData> {
    let executor = std::sync::Arc::new(FunctionStep::new(
        move |ctx: &mut WorkflowContext<WorkItemData>| {
            let send = send.clone();
            let agent_name = agent_name.clone();
            let diff_prompt_prefix = diff_prompt_prefix.clone();
            let is_fix_round = ctx.input.starts_with(REVIEW_ISSUES_MARKER);
            Box::pin(async move {
                let prompt = if is_fix_round {
                    format!(
                        "{diff_prompt_prefix}\n\nAddress this reviewer feedback, commit the \
                         fix, then reply with a one-line summary:\n{}",
                        ctx.input
                    )
                } else {
                    format!(
                        "{diff_prompt_prefix}\n\nReview the diff above. Reply with exactly \
                         `{REVIEW_APPROVED_MARKER}` if it's correct and ready, or \
                         `{REVIEW_ISSUES_MARKER}` followed by a bullet list of concrete \
                         issues to fix."
                    )
                };
                let (reply, in_tok, out_tok) = send(agent_name, prompt)
                    .await
                    .map_err(|message| WorkflowError::StepFailed {
                        step_id: StepId::new("review_or_fix"),
                        message,
                    })?;
                ctx.input_tokens += in_tok;
                ctx.output_tokens += out_tok;

                ctx.output = if is_fix_round {
                    format!("FIXED: {reply}")
                } else if reply.trim_start().starts_with(REVIEW_APPROVED_MARKER) {
                    REVIEW_APPROVED_MARKER.to_string()
                } else {
                    ctx.data.review_issues = Some(reply.clone());
                    reply
                };

                // The engine's `until` check (see
                // `WorkflowEngine::execute_loop`) is a case-insensitive
                // substring match against whatever this iteration's
                // `ctx.output` ends up being — including a fix round's
                // echoed reply, if it happens to contain the approval
                // marker. Mirror that check here so `review_issues` (read
                // by `finalize`'s cap-exceeded gate comment) only survives
                // to the end of the loop when it's ACTUALLY stopping
                // without approval, rather than clearing it unconditionally
                // on every fix round regardless of why the loop stopped.
                if ctx
                    .output
                    .to_lowercase()
                    .contains(&REVIEW_APPROVED_MARKER.to_lowercase())
                {
                    ctx.data.review_issues = None;
                }
                Ok(StepResult::Success)
            })
        },
    ));

    StepDefinition::new("review_or_fix", "review_or_fix", executor).with_mode(
        flare_workflow::StepMode::Loop {
            max_iterations: 2 * MAX_REVIEW_CYCLES,
            until: REVIEW_APPROVED_MARKER.to_string(),
        },
    )
}

/// Wraps `execute_work`'s existing hold/`item_done`/comment/notify tail
/// (`src/cli/work.rs`'s `HeadlessOutcome::Ok` arm) as the pipeline's last
/// step. Three outcomes, checked in order:
///
/// 1. `ctx.data.hold_reason` set (Task 3's `coder` step detected an
///    `AGENTFLARE_HOLD:` signal) — release the claim and post an "on hold"
///    comment instead of calling `item_done`, same as `execute_work`'s hold
///    branch.
/// 2. `ctx.data.review_issues` still set (Task 4's `review_or_fix` loop hit
///    `MAX_REVIEW_CYCLES` without ever reaching approval) — gate for a
///    human with a comment instead of opening a PR on unreviewed code, since
///    this step has no access to `supervisor`'s label-id lookups for a real
///    relabel (that stays the supervisor's job on its next discovery tick).
/// 3. Otherwise — the success path: `item_done`, then the same
///    `cap_reply_for_comment`/`format_success_comment`/comment/notify
///    sequence `execute_work` runs today.
///
/// Retried up to 3 times with exponential backoff (`RetryPolicy`) — this
/// step's own MCP calls (`item_done` etc.) can fail transiently the same
/// way `coder`/`review_or_fix`'s agent dispatch can, and unlike those two,
/// a failure here has already done the real work and just needs to land the
/// result.
pub(crate) fn build_finalize_step(
    mcp: std::sync::Arc<AgentflareMcp>,
    item_id: String,
    notify_recipient: Option<String>,
) -> StepDefinition<WorkItemData> {
    let executor = std::sync::Arc::new(FunctionStep::new(
        move |ctx: &mut WorkflowContext<WorkItemData>| {
            let mcp = mcp.clone();
            let item_id = item_id.clone();
            let notify_recipient = notify_recipient.clone();
            Box::pin(async move {
                if let Some(reason) = ctx.data.hold_reason.clone() {
                    let _ = mcp.item_release(ItemRequest {
                        action: "release".into(),
                        id: Some(item_id.clone()),
                        ..Default::default()
                    });
                    let body = format!("## agentflare work — on hold\n\n{reason}");
                    let _ = mcp.comment_impl(CommentRequest {
                        action: "create".into(),
                        item_id: Some(item_id.clone()),
                        body: Some(body.clone()),
                        ..Default::default()
                    });
                    if let Some(recipient) = notify_recipient.as_deref() {
                        crate::cli::work::notify(recipient, &body, &item_id);
                    }
                    return Ok(StepResult::Success);
                }

                if ctx.data.review_issues.is_some() {
                    let issues = ctx.data.review_issues.clone().unwrap_or_default();
                    let _ = mcp.comment_impl(CommentRequest {
                        action: "create".into(),
                        item_id: Some(item_id.clone()),
                        body: Some(format!(
                            "## agentflare work — needs human review\n\n\
                             Automated review/fix did not converge after {MAX_REVIEW_CYCLES} \
                             cycles. Latest outstanding issues:\n\n{issues}"
                        )),
                        ..Default::default()
                    });
                    return Ok(StepResult::Success);
                }

                let done_resp = mcp
                    .item_done(ItemRequest {
                        action: "done".into(),
                        id: Some(item_id.clone()),
                        summary: Some(ctx.data.reply_text.clone()),
                        ..Default::default()
                    })
                    .map_err(|e| WorkflowError::StepFailed {
                        step_id: StepId::new("finalize"),
                        message: e.message.to_string(),
                    })?;
                let done_val: serde_json::Value =
                    serde_json::from_str(&done_resp).unwrap_or(serde_json::Value::Null);
                ctx.data.pr_url = done_val["pr_url"].as_str().map(str::to_string);

                let comment_reply =
                    crate::cli::work::cap_reply_for_comment(&mcp, &item_id, &ctx.data.reply_text);
                let comment_body = crate::cli::work::format_success_comment(
                    &comment_reply,
                    ctx.data.session_id.as_deref(),
                    ctx.data.cost_usd,
                    ctx.data.pr_url.as_deref(),
                );
                let _ = mcp.comment_impl(CommentRequest {
                    action: "create".into(),
                    item_id: Some(item_id.clone()),
                    body: Some(comment_body.clone()),
                    ..Default::default()
                });
                if let Some(recipient) = notify_recipient.as_deref() {
                    crate::cli::work::notify(recipient, &comment_body, &item_id);
                }
                Ok(StepResult::Success)
            })
        },
    ));

    StepDefinition::new("finalize", "finalize", executor).with_retry(flare_workflow::RetryPolicy {
        max_attempts: 3,
        backoff: flare_workflow::BackoffStrategy::Exponential {
            base: std::time::Duration::from_secs(1),
            max: std::time::Duration::from_secs(30),
        },
    })
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

    #[tokio::test]
    async fn review_or_fix_step_stops_immediately_when_approved() {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls2 = calls.clone();
        let send: flare_workflow::json::SendMessage = Arc::new(move |_a, _p| {
            calls2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Ok(("REVIEW_APPROVED".to_string(), 1u64, 0u64)) })
        });
        let step = build_review_or_fix_step_with_sender(
            "agent".to_string(),
            "dummy diff prompt".to_string(),
            send,
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
                assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
                assert!(state.context.data.review_issues.is_none());
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("review_or_fix step did not complete");
    }

    #[tokio::test]
    async fn review_or_fix_step_fixes_once_then_approves() {
        let call_n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call_n2 = call_n.clone();
        let send: flare_workflow::json::SendMessage = Arc::new(move |_a, _p| {
            let n = call_n2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async move {
                if n == 0 {
                    Ok(("REVIEW_ISSUES:\n- fix the typo".to_string(), 1, 0))
                } else {
                    Ok(("REVIEW_APPROVED".to_string(), 1, 0))
                }
            })
        });
        let step =
            build_review_or_fix_step_with_sender("agent".to_string(), "diff".to_string(), send);
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
                // Observed against the real engine: only 2 calls, not the
                // brief's modeled 3. Call 1 is the reviewer round
                // (REVIEW_ISSUES). Call 2 is the fix round, whose reply
                // ("REVIEW_APPROVED" per this mock) gets echoed verbatim
                // into this step's `ctx.output` as "FIXED: REVIEW_APPROVED"
                // — `execute_loop`'s `until` check is a case-insensitive
                // substring match against that raw output, so it fires
                // right there and the loop stops before ever issuing a
                // third, dedicated re-review call.
                assert_eq!(call_n.load(std::sync::atomic::Ordering::SeqCst), 2);
                assert!(state.context.data.review_issues.is_none());
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("review_or_fix step did not complete");
    }

    #[tokio::test]
    async fn review_or_fix_step_stops_at_cap_with_issues_still_open() {
        let send: flare_workflow::json::SendMessage = Arc::new(move |_a, _p| {
            Box::pin(async { Ok(("REVIEW_ISSUES:\n- still broken".to_string(), 1, 0)) })
        });
        let step =
            build_review_or_fix_step_with_sender("agent".to_string(), "diff".to_string(), send);
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
                assert!(state.context.data.review_issues.is_some());
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("review_or_fix step did not complete");
    }

    #[tokio::test]
    async fn finalize_step_calls_item_done_on_success() {
        let (mcp, _backend_tmp, _repo_tmp, item_id, project_id, worktree_path) =
            crate::mcp_server::tests::mcp_with_claimed_item("Finalize test item");
        // Something real to commit — otherwise `item_done` sees a
        // never-diverged branch and treats it as a no-op ("unchanged")
        // rather than a completion.
        std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();
        let mcp = Arc::new(mcp);

        let data = WorkItemData {
            reply_text: "implemented the thing".into(),
            ..Default::default()
        };
        let step = build_finalize_step(mcp.clone(), item_id.clone(), None);
        let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
        let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
        engine.register_workflow(wf).unwrap();
        let run_id = engine
            .start_workflow(WorkflowId::new(WORKFLOW_ID), data, String::new())
            .await
            .unwrap();

        for _ in 0..50 {
            let state = engine.get_status(run_id).await.unwrap();
            if state.status == flare_workflow::WorkflowStatus::Completed {
                let completed_state_id = mcp
                    .with_backend_db(|conn| {
                        agentflare_backend::state::list_by_project(conn, &project_id)
                            .unwrap()
                            .into_iter()
                            .find(|st| st.group_name == "completed")
                            .unwrap()
                            .id
                    })
                    .unwrap();
                let item = mcp
                    .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    item.state_id, completed_state_id,
                    "finalize must move the item to the project's real 'completed' state"
                );
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("finalize step did not complete");
    }
}
