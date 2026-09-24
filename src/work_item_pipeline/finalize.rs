/// Wraps `execute_work`'s existing hold/`item_done`/comment/notify tail
/// (`src/cli/work.rs`'s `HeadlessOutcome::Ok` arm) as the pipeline's last
/// step. Four outcomes, checked in order:
///
/// 1. `ctx.data.hold_reason` set (Task 3's `coder` step detected an
///    `AGENTFLARE_HOLD:` signal) — release the claim and post an "on hold"
///    comment instead of calling `item_done`, same as `execute_work`'s hold
///    branch.
/// 2. `ctx.data.review_only` set and `ctx.data.design_spec` unset (item #507
///    — the dispatched item asked for analysis, not implementation) —
///    release the claim and post the accumulated findings as a comment
///    instead of ever reaching `item_done`/PR flow, regardless of
///    `review_issues` state. A design-spec task (item #216) skips this
///    branch even though it's also `review_only`: its deliverable is a
///    written spec file, a real artifact that needs to be committed and
///    land in a PR via the success path below, not just described in a
///    comment.
/// 3. `ctx.data.review_issues` still set (Task 4's `review_or_fix` loop hit
///    `MAX_REVIEW_CYCLES` without ever reaching approval) — gate for a
///    human with a comment instead of opening a PR on unreviewed code, since
///    this step has no access to `supervisor`'s label-id lookups for a real
///    relabel (that stays the supervisor's job on its next discovery tick).
///    The job is finished either way — release the claim so redispatch /
///    supervisor discovery can pick the item back up.
/// 4. Otherwise — the success path: `item_done`, then the same
///    `cap_reply_for_comment`/`format_success_comment`/comment/notify
///    sequence `execute_work` runs today.
///
/// Retried up to 3 times with exponential backoff (`RetryPolicy`) — this
/// step's own MCP calls (`item_done` etc.) can fail transiently the same
/// way `coder`/`review_or_fix`'s agent dispatch can, and unlike those two,
/// a failure here has already done the real work and just needs to land the
/// result.
///
/// Best-effort claim release on every terminal success except when
/// `item_done` deliberately left the lease held for an open PR (`in_review`).
fn finalize_release_claim_best_effort(
    mcp: &crate::mcp_server::AgentflareMcp,
    item_id: &str,
    leave_claim_held: bool,
) {
    if leave_claim_held {
        return;
    }
    let _ = mcp.item_release(ItemRequest {
        action: "release".into(),
        id: Some(item_id.to_string()),
        ..Default::default()
    });
}

/// `item_id`/`notify_recipient`/`owner` are read from `ctx.data` at
/// execution time (not closed over here) so a run resumed by
/// `engine().recover()` after a crash calls `item_done`/`item_release`
/// against the real item the crashed run persisted, not an empty
/// placeholder. `mcp` stays a registration-time closure — it's a generic
/// backend handle (lazily opens the real DB on first use), not per-item
/// state, so it's safe to share across every run.
pub(crate) fn build_finalize_step(
    mcp: std::sync::Arc<AgentflareMcp>,
) -> StepDefinition<WorkItemData> {
    let executor = std::sync::Arc::new(FunctionStep::new(
        move |ctx: &mut WorkflowContext<WorkItemData>| {
            let mcp = mcp.clone();
            Box::pin(async move {
                // Read at execution time, not closed over at
                // step-registration time -- see `WorkItemData::item_id`'s
                // doc comment. An empty id means identity genuinely
                // couldn't be reconstructed (e.g. a run started before this
                // field existed) -- fail closed rather than guess, same as
                // this used to fail (by erroring inside `item_done`) when
                // the boot-time recovery definition closed over a
                // placeholder id.
                if ctx.data.item_id.is_empty() {
                    return Ok(StepResult::Failed(
                        "finalize: item_id is empty, cannot reconstruct run identity".to_string(),
                    ));
                }
                // A job cancelled by a reassignment (item #607) must not push
                // or open a PR for an item that now belongs to another agent.
                let cancel_owner = ctx.data.owner.clone();
                let cancelled = tokio::task::spawn_blocking(move || {
                    crate::agent_launch::owner_job_cancelled(&cancel_owner)
                })
                .await
                .unwrap_or(false);
                if cancelled {
                    return Ok(StepResult::Failed(
                        agentflare_jobs::cancel::CANCELLED_MESSAGE.to_string(),
                    ));
                }
                let item_id = ctx.data.item_id.clone();
                let notify_recipient = ctx.data.notify_recipient.clone();
                let owner = ctx.data.owner.clone();
                crate::claims::with_owner_override(owner, || {
                    if let Some(reason) = ctx.data.hold_reason.clone() {
                        finalize_release_claim_best_effort(&mcp, &item_id, false);
                        let body = format!("## agentflare work — on hold\n\n{reason}");
                        mcp.post_item_comment(&item_id, &body);
                        if let Some(recipient) = notify_recipient.as_deref() {
                            crate::cli::work::notify(recipient, &body, &item_id);
                        }
                        return Ok(StepResult::Success);
                    }

                    if ctx.data.review_only && !ctx.data.design_spec {
                        let findings = if ctx.data.review_findings.is_empty() {
                            ctx.data
                                .last_report
                                .clone()
                                .or_else(|| ctx.data.review_issues.clone())
                                .unwrap_or_else(|| "No findings reported.".to_string())
                        } else {
                            ctx.data.review_findings.join("\n\n---\n\n")
                        };
                        finalize_release_claim_best_effort(&mcp, &item_id, false);
                        let body = format!("## agentflare work — review findings\n\n{findings}");
                        mcp.post_item_comment(&item_id, &body);
                        if let Some(recipient) = notify_recipient.as_deref() {
                            crate::cli::work::notify(recipient, &body, &item_id);
                        }
                        return Ok(StepResult::Success);
                    }

                    if ctx.data.review_issues.is_some() {
                        let issues = ctx.data.review_issues.clone().unwrap_or_default();
                        finalize_release_claim_best_effort(&mcp, &item_id, false);
                        mcp.post_item_comment(
                            &item_id,
                            format!(
                                "## agentflare work — needs human review\n\n\
                             Automated review/fix did not converge after {MAX_REVIEW_CYCLES} \
                             cycles. Latest outstanding issues:\n\n{issues}"
                            ),
                        );
                        return Ok(StepResult::Success);
                    }

                    // A correction landed too late for any task turn to
                    // consume it (posted after the last `sdd_loop` iteration
                    // ran, e.g. during the final turn itself) — gate the
                    // same way `hold_reason` above does rather than silently
                    // opening a PR the correction says not to (item
                    // #269/#270's whole premise: PR #753 shipped exactly the
                    // approach a comment said to disregard).
                    if !ctx.data.pending_corrections.is_empty() {
                        let corrections = ctx.data.pending_corrections.join("\n\n---\n\n");
                        finalize_release_claim_best_effort(&mcp, &item_id, false);
                        let body = format!(
                            "## agentflare work — on hold\n\n\
                             Unread correction posted after this task started -- needs a \
                             fresh pass before this item can be marked done:\n\n{corrections}"
                        );
                        mcp.post_item_comment(&item_id, &body);
                        if let Some(recipient) = notify_recipient.as_deref() {
                            crate::cli::work::notify(recipient, &body, &item_id);
                        }
                        return Ok(StepResult::Success);
                    }

                    // Squash checkpoint commits (item #193) into one diff
                    // before `item_done`'s own commit, so the LOC-freeze
                    // gate sees the whole run at once. `.take()`: a retry
                    // of this step must not re-squash an already-squashed
                    // commit.
                    if let Some(base_sha) = ctx.data.checkpoint_base_sha.take()
                        && !ctx.data.worktree_path.is_empty()
                    {
                        let worktree_path = std::path::PathBuf::from(&ctx.data.worktree_path);
                        if let Err(e) = crate::worktree::squash_since(&worktree_path, &base_sha) {
                            eprintln!(
                                "finalize: squashing sdd_loop checkpoint commits for item {item_id} failed: {e}"
                            );
                        }
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
                    // "unchanged": nothing was ever committed on the item's
                    // branch, so `item_done` released the claim and left the
                    // item in "started" -- no PR, no completion. Reporting
                    // that as success left the item parked there with no
                    // claim, no `ready-for-work` label and no job, forever.
                    // Fail the run instead, so the terminal-failure hook
                    // re-arms it (bounded by the dispatch failure ceiling).
                    // `StepResult::Failed`, not an error: a retry of this
                    // step would just find the same empty branch again.
                    if done_val["status"].as_str() == Some("unchanged") {
                        return Ok(StepResult::Failed(format!(
                            "finalize: item {item_id} has no committed changes on its branch -- \
                             nothing to publish, so it was not marked done"
                        )));
                    }
                    ctx.data.pr_url = done_val["pr_url"].as_str().map(str::to_string);
                    let leave_claim_held = done_val["status"].as_str() == Some("in_review");
                    finalize_release_claim_best_effort(&mcp, &item_id, leave_claim_held);

                    let comment_reply = crate::cli::work::cap_reply_for_comment(
                        &mcp,
                        &item_id,
                        &ctx.data.reply_text,
                    );
                    let comment_body = crate::cli::work::format_success_comment(
                        &comment_reply,
                        ctx.data.session_id.as_deref(),
                        ctx.data.cost_usd,
                        ctx.data.pr_url.as_deref(),
                    );
                    mcp.post_item_comment(&item_id, &comment_body);
                    if let Some(recipient) = notify_recipient.as_deref() {
                        crate::cli::work::notify(recipient, &comment_body, &item_id);
                    }
                    Ok(StepResult::Success)
                })
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
