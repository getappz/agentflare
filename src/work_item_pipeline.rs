//! Builds and runs the per-work-item `flare-workflow` pipeline: a judge-driven
//! `sdd_loop` (subagent-driven-development task loop) → `finalize`. See
//! `docs` item #110 for the original design and the durable-sdd-workflow
//! plan for the `sdd_loop` step that replaced its earlier `coder`/
//! `review_or_fix` steps; corrected against the crate's real Rust builder
//! API (not the JSON/OpenFang schema — `finalize` runs real Rust logic, not
//! an agent prompt).

/// Cap on review/fix cycles before an item is gated for a human instead of
/// looping forever on an agent that can't converge. Mirrors
/// `quota::decide::SELF_REPAIR_CAP`'s existing cap-constant pattern.
pub(crate) const MAX_REVIEW_CYCLES: u32 = 3;

/// `flare_workflow::WorkflowId` name for this pipeline definition —
/// registered once at daemon boot (see `src/dashboard/server.rs`) and
/// referenced by every dispatched item's run.
pub(crate) const WORKFLOW_ID: &str = "agentflare-work-item";

/// Represents a single task within an SDD (subagent-driven-development) workflow.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct SddTask {
    pub id: usize,
    pub title: String,
    pub body: String,
    pub model_tier: Option<TaskModelTier>,
}

/// Model capability tier for an SDD task — determines agent dispatch preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskModelTier {
    Mechanical,
    Integration,
    Architecture,
}

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
    /// SDD workflow: list of tasks to be executed.
    pub tasks: Vec<SddTask>,
    /// SDD workflow: index of the current task being processed.
    pub current_task_index: usize,
    /// SDD workflow: count of fix rounds applied to the current task.
    pub fix_round: u32,
    /// SDD workflow: audit log of task lifecycle events.
    pub ledger: Vec<String>,
    /// SDD workflow: latest generated report, if any.
    pub last_report: Option<String>,
}

impl flare_workflow::WorkflowData for WorkItemData {
    fn workflow_type() -> &'static str {
        WORKFLOW_ID
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum JudgeAction {
    ContinueTask,
    FixRound,
    Escalate,
    ParkFinding,
    RuleAndContinue,
    InsertTask,
    SkipTask,
    AdvanceTask,
    CompletePipeline,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct JudgeDecision {
    pub action: JudgeAction,
    pub rationale: String,
    pub ledger_line: String,
    pub task_model_tier: Option<TaskModelTier>,
}

#[derive(Debug)]
pub(crate) enum JudgeParseError {
    InvalidJson(String),
}

impl std::fmt::Display for JudgeParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JudgeParseError::InvalidJson(msg) => {
                write!(f, "judge reply is not valid decision JSON: {msg}")
            }
        }
    }
}

/// The first fenced code block in `reply` (` ```json ... ``` ` or a bare
/// ` ``` ... ``` `), if any -- an explicit fence is an unambiguous boundary
/// the judge only produces on purpose, so it's tried before brace-scanning.
fn extract_fenced_block(reply: &str) -> Option<&str> {
    let after_open = reply.find("```")? + 3;
    let rest = &reply[after_open..];
    // Skip an optional language tag (e.g. `json`) up to the fence's newline.
    let body_start = rest.find('\n').map(|i| i + 1).unwrap_or(0);
    let body = &rest[body_start..];
    let end = body.find("```")?;
    Some(body[..end].trim())
}

/// Scans forward from the first `{` for its own matching `}`, tracking
/// nesting depth and skipping brace-like bytes inside JSON string literals
/// (so a `{`/`}` embedded in a string value, or in unrelated commentary
/// after the object, can't extend or corrupt the span). Returns the first
/// complete top-level object instead of naively spanning from the first `{`
/// to the *last* `}` anywhere in the reply, which a second unrelated
/// brace-shaped span later in the text could throw off.
fn extract_first_balanced_object(reply: &str) -> Option<&str> {
    let start = reply.find('{')?;
    let bytes = reply.as_bytes();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&reply[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The judge is prompted to reply with exactly one JSON object; this
/// tolerates a reply that wraps the object in prose, a fenced code block, or
/// trailing commentary containing its own unrelated braces, but does not
/// otherwise repair malformed JSON — a genuine parse failure (including
/// syntactically valid JSON missing a required field) is a step Failure,
/// retried by the step's own RetryPolicy.
pub(crate) fn parse_judge_decision(reply: &str) -> Result<JudgeDecision, JudgeParseError> {
    if let Some(fenced) = extract_fenced_block(reply)
        && let Ok(decision) = serde_json::from_str(fenced)
    {
        return Ok(decision);
    }
    let candidate = extract_first_balanced_object(reply).ok_or_else(|| {
        JudgeParseError::InvalidJson("no balanced '{...}' object found".to_string())
    })?;
    serde_json::from_str(candidate).map_err(|e| JudgeParseError::InvalidJson(e.to_string()))
}

use flare_workflow::executor::FunctionStep;
use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::{
    StepDefinition, StepId, StepResult, WorkflowContext, WorkflowEngine, WorkflowError, WorkflowId,
    WorkflowStatus,
};
use std::str::FromStr;

use crate::mcp_server::AgentflareMcp;
use crate::mcp_server::types::{CommentRequest, ItemRequest};

/// Marker the reviewer replies with when the diff is approved — matched
/// (case-insensitive substring) by the `StepMode::Loop`'s `until` field to
/// stop the loop.
const REVIEW_APPROVED_MARKER: &str = "REVIEW_APPROVED";
/// Prefix the reviewer replies with when there are unresolved issues —
/// echoed back as `ctx.input` on the next iteration, which is how the
/// closure below tells reviewer-turn from fixer-turn apart.
const REVIEW_ISSUES_MARKER: &str = "REVIEW_ISSUES:";

/// Cap on fix rounds for a single SDD task before the loop gives up on it —
/// mirrors `MAX_REVIEW_CYCLES`'s existing cap-constant pattern for the
/// `coder`/`review_or_fix` pipeline.
pub(crate) const MAX_FIX_ROUNDS: u32 = 5;
/// Safety ceiling on how many tasks a single SDD run can process — plans
/// realistically have far fewer tasks; this bounds `insert_task` from
/// producing an unbounded plan.
pub(crate) const MAX_TASKS_PROCESSED: usize = 50;

/// Marker `ctx.output` is set to when the SDD loop's judge decides the
/// whole plan is done — the `StepMode::Loop`'s `until` field this step is
/// registered with (see the bottom of this function).
const SDD_PIPELINE_COMPLETE_MARKER: &str = "PIPELINE_COMPLETE";

/// The single `StepMode::Loop` step for the SDD (subagent-driven-development)
/// task pipeline. Each iteration:
///
/// 1. Reads `ctx.data` to decide which role to dispatch this turn —
///    implementer (fresh task or mid-fix-round), task-reviewer (implementer
///    just reported and no re-review is in progress), or re-reviewer (a fix
///    landed for previously recorded findings) — and sends that role's
///    prompt to `agent_name` via `send`.
/// 2. Sends the judge its own prompt (task list, ledger, latest role reply)
///    to `judge_agent_name` via `send`, and parses its JSON decision.
/// 3. Applies the decision to `ctx.data` (advance/skip the task, bump the
///    fix round, insert a new task, or terminate the pipeline).
///
/// Mirrors `build_review_or_fix_step_with_sender`'s shape: a `FunctionStep`
/// closure over an injected `send`, same test seam pattern.
pub(crate) fn build_sdd_loop_step(
    agent_name: String,
    judge_agent_name: String,
    send: flare_workflow::json::SendMessage,
) -> StepDefinition<WorkItemData> {
    let executor = std::sync::Arc::new(FunctionStep::new(
        move |ctx: &mut WorkflowContext<WorkItemData>| {
            let send = send.clone();
            let agent_name = agent_name.clone();
            let judge_agent_name = judge_agent_name.clone();
            Box::pin(async move {
                if ctx.data.current_task_index >= MAX_TASKS_PROCESSED {
                    return Ok(StepResult::Failure);
                }
                if ctx.data.tasks.is_empty() || ctx.data.current_task_index >= ctx.data.tasks.len()
                {
                    ctx.output = SDD_PIPELINE_COMPLETE_MARKER.to_string();
                    return Ok(StepResult::Success);
                }

                let task = ctx.data.tasks[ctx.data.current_task_index].clone();

                // 1. Role dispatch — state read from ctx.data decides which
                // role plays this turn.
                // Reviewer branches (task-reviewer, re-reviewer) dispatch on
                // `judge_agent_name`, not `agent_name` — that's the agent
                // reserved for every non-implementer role (see the judge
                // dispatch further down), so a usage-threshold fallback that
                // swaps `agent_name` to another CLI still leaves real code
                // review running on the reserved agent.
                let (role_agent, role_prompt) = if ctx.data.review_issues.is_some() {
                    if ctx.data.last_report.is_some() {
                        // A fix has already been submitted for the current
                        // issues — re-review it.
                        let findings = ctx.data.review_issues.clone().unwrap_or_default();
                        let fix_report = ctx.data.last_report.clone().unwrap_or_default();
                        (
                            judge_agent_name.clone(),
                            build_re_reviewer_prompt(&task, &findings, &fix_report),
                        )
                    } else {
                        // Issues open, no fix attempt yet — dispatch the
                        // implementer to fix them.
                        let fix_context = ctx.data.review_issues.as_deref();
                        (
                            agent_name.clone(),
                            build_implementer_prompt(&task, fix_context),
                        )
                    }
                } else if ctx.data.last_report.is_some() {
                    // No open issues; a report is pending review.
                    let report = ctx.data.last_report.clone().unwrap_or_default();
                    (
                        judge_agent_name.clone(),
                        build_task_reviewer_prompt(&task, &report),
                    )
                } else {
                    // Fresh task, nothing dispatched yet.
                    (agent_name.clone(), build_implementer_prompt(&task, None))
                };

                let (role_reply, in_tok, out_tok) = send(
                    flare_workflow::json::StepInvocation::simple(role_agent, role_prompt),
                )
                .await
                .map_err(|message| WorkflowError::StepFailed {
                    step_id: StepId::new("sdd_loop"),
                    message,
                })?;
                ctx.input_tokens += in_tok;
                ctx.output_tokens += out_tok;

                if let Some(issues) = role_reply.strip_prefix(REVIEW_ISSUES_MARKER) {
                    ctx.data.review_issues = Some(issues.trim().to_string());
                    ctx.data.last_report = None;
                } else if role_reply.trim() == REVIEW_APPROVED_MARKER {
                    ctx.data.review_issues = None;
                } else {
                    ctx.data.last_report = Some(role_reply.clone());
                }

                // 2. Judge dispatch.
                let judge_prompt = build_judge_prompt(
                    &ctx.data.tasks,
                    ctx.data.current_task_index,
                    &ctx.data.ledger,
                    &role_reply,
                );
                let (judge_reply, jin_tok, jout_tok) = send(
                    flare_workflow::json::StepInvocation::simple(judge_agent_name, judge_prompt),
                )
                .await
                .map_err(|message| WorkflowError::StepFailed {
                    step_id: StepId::new("sdd_loop"),
                    message,
                })?;
                ctx.input_tokens += jin_tok;
                ctx.output_tokens += jout_tok;

                let decision = match parse_judge_decision(&judge_reply) {
                    Ok(d) => d,
                    // A malformed judge reply is exactly the kind of
                    // transient hiccup this step's `RetryPolicy` exists for
                    // — but the engine only consults the policy for a real
                    // `Err`, never for `Ok(StepResult::Failure)` (see
                    // `execute_step_with_retry` in
                    // `crates/flare-workflow/src/engine.rs`, which hardcodes
                    // `should_retry = false` for the latter). Surfacing this
                    // as `StepFailed` is what actually gets it retried.
                    Err(e) => {
                        return Err(WorkflowError::StepFailed {
                            step_id: StepId::new("sdd_loop"),
                            message: e.to_string(),
                        });
                    }
                };

                // 3. Apply the decision.
                ctx.data.ledger.push(decision.ledger_line.clone());
                match decision.action {
                    JudgeAction::FixRound => {
                        ctx.data.fix_round += 1;
                        if ctx.data.fix_round > MAX_FIX_ROUNDS {
                            return Ok(StepResult::Failure);
                        }
                    }
                    JudgeAction::Escalate => {
                        ctx.data.fix_round += 1;
                    }
                    JudgeAction::AdvanceTask | JudgeAction::SkipTask => {
                        ctx.data.current_task_index += 1;
                        ctx.data.fix_round = 0;
                        ctx.data.review_issues = None;
                        ctx.data.last_report = None;
                    }
                    JudgeAction::InsertTask => {
                        if ctx.data.tasks.len() >= MAX_TASKS_PROCESSED {
                            return Ok(StepResult::Failure);
                        }
                        let new_id = ctx.data.tasks.len();
                        ctx.data.tasks.push(SddTask {
                            id: new_id,
                            title: decision.rationale.clone(),
                            body: decision.rationale.clone(),
                            model_tier: decision.task_model_tier,
                        });
                    }
                    JudgeAction::CompletePipeline => {
                        ctx.output = SDD_PIPELINE_COMPLETE_MARKER.to_string();
                        return Ok(StepResult::Success);
                    }
                    JudgeAction::ContinueTask
                    | JudgeAction::ParkFinding
                    | JudgeAction::RuleAndContinue => {}
                }

                ctx.output = "CONTINUE".to_string();
                Ok(StepResult::Success)
            })
        },
    ));

    StepDefinition::new("sdd_loop", "SDD task loop", executor)
        .with_mode(flare_workflow::StepMode::Loop {
            max_iterations: (MAX_TASKS_PROCESSED as u32) * (MAX_FIX_ROUNDS + 2),
            until: SDD_PIPELINE_COMPLETE_MARKER.to_string(),
        })
        .with_retry(flare_workflow::RetryPolicy {
            max_attempts: 3,
            backoff: flare_workflow::BackoffStrategy::Exponential {
                base: std::time::Duration::from_secs(1),
                max: std::time::Duration::from_secs(30),
            },
        })
        // `flare_workflow::WorkflowDefinition::new`'s builder default
        // (300s) is `execute_loop`'s per-iteration cap (`loops.rs` wraps
        // each iteration's `send` calls -- implementer/reviewer AND judge --
        // in `tokio::time::timeout(step_timeout, ..)`), not a cap on the
        // whole loop. A real implementer/reviewer/judge dispatch can
        // legitimately run for the same order of magnitude as
        // `supervisor::WORK_JOB_TIMEOUT_SECS` (the outer job's own hard-cap,
        // itself sized off `WorkArgs::DEFAULT_TIMEOUT_SECS` plus margin --
        // see that constant's doc comment), so reuse it here rather than
        // leaving the 300s library default in place: anything shorter than
        // the outer job timeout just moves the premature-kill point inward
        // without fixing it.
        .with_timeout(std::time::Duration::from_secs(
            crate::supervisor::WORK_JOB_TIMEOUT_SECS,
        ))
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
    owner: String,
) -> StepDefinition<WorkItemData> {
    let executor = std::sync::Arc::new(FunctionStep::new(
        move |ctx: &mut WorkflowContext<WorkItemData>| {
            let mcp = mcp.clone();
            let item_id = item_id.clone();
            let notify_recipient = notify_recipient.clone();
            let owner = owner.clone();
            Box::pin(async move {
                crate::claims::with_owner_override(owner, || {
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

/// Assembles the full `sdd_loop` → `finalize` pipeline as a registerable
/// `WorkflowDefinition`. Real entry point: dispatches through
/// [`real_agent_send_hook`] — see `build_work_item_pipeline_with_sender` for
/// the test seam.
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_work_item_pipeline(
    agent: agent_registry::Agent,
    item_description: String,
    plan_doc: Option<String>,
    mcp: std::sync::Arc<AgentflareMcp>,
    item_id: String,
    owner: String,
    notify_recipient: Option<String>,
    timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
    extra_args: Vec<String>,
) -> flare_workflow::WorkflowDefinition<WorkItemData> {
    build_work_item_pipeline_with_sender(
        agent,
        item_description,
        plan_doc,
        mcp,
        item_id,
        owner,
        notify_recipient,
        real_agent_send_hook(timeout, idle_timeout, extra_args),
    )
}

/// Same headless-agent wiring as `crate::workflow::agent_send_hook()`
/// (`spawn_blocking`-wrapped `agent_launch::run_headless`), but honoring
/// `execute_work`'s caller-supplied `--timeout`/`--idle-timeout`/
/// `--max-turns`/`--max-cost-usd`/`--model` flags instead of that hook's
/// hardcoded 600s/300s/no-extra-args, which are tuned for the JSON pipeline
/// feature it serves (`src/workflow.rs`), not a work-item dispatch — using
/// it unmodified here would silently drop those flags on every daemon- and
/// CLI-dispatched item.
fn real_agent_send_hook(
    timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
    extra_args: Vec<String>,
) -> flare_workflow::json::SendMessage {
    std::sync::Arc::new(move |inv: flare_workflow::json::StepInvocation| {
        let extra_args = extra_args.clone();
        let flare_workflow::json::StepInvocation { agent, prompt, .. } = inv;
        Box::pin(async move {
            let outcome = tokio::task::spawn_blocking(move || {
                crate::agent_launch::run_headless(
                    agent_registry::REGISTRY,
                    &agent,
                    &prompt,
                    timeout,
                    idle_timeout,
                    &extra_args,
                )
            })
            .await
            .map_err(|e| format!("agent task panicked: {e}"))?;
            match outcome {
                crate::agent_launch::HeadlessOutcome::Ok(reply) => Ok((reply, 0, 0)),
                crate::agent_launch::HeadlessOutcome::UnknownAgent(e)
                | crate::agent_launch::HeadlessOutcome::NotHeadless(e)
                | crate::agent_launch::HeadlessOutcome::NotFound(e)
                | crate::agent_launch::HeadlessOutcome::Failed(e) => Err(e),
            }
        })
    })
}

/// Test seam: same pipeline, one injected `SendMessage` shared by the
/// `sdd_loop` step's implementer/reviewer/judge roles instead of the real
/// headless agent hook (mirrors `build_sdd_loop_step`'s own test seam
/// pattern). `item_description`/`plan_doc` aren't consumed directly here —
/// they exist so this function's signature stays parallel to
/// `run_or_resume_with_sender`'s, which is the caller that actually needs
/// them (to compute `WorkItemData::tasks` via `load_or_synthesize_tasks`
/// before `start_workflow`; see that function).
#[allow(clippy::too_many_arguments)]
fn build_work_item_pipeline_with_sender(
    agent: agent_registry::Agent,
    _item_description: String,
    _plan_doc: Option<String>,
    mcp: std::sync::Arc<AgentflareMcp>,
    item_id: String,
    owner: String,
    notify_recipient: Option<String>,
    send: flare_workflow::json::SendMessage,
) -> flare_workflow::WorkflowDefinition<WorkItemData> {
    let agent_name = agent.as_str().to_string();
    let sdd_loop = build_sdd_loop_step(agent_name.clone(), agent_name, send);
    let finalize =
        build_finalize_step(mcp, item_id, notify_recipient, owner).depends_on(&["sdd_loop"]);

    flare_workflow::WorkflowDefinition::new(WORKFLOW_ID, "sdd work item")
        .add_step(sdd_loop)
        .add_step(finalize)
}

/// Process-lifetime shared engine — built once, reused by every dispatch
/// AND by the boot-time `recover()` sweep (Task 8), so a run resumed at
/// startup and a later `run_or_resume` call for a DIFFERENT item share the
/// same registered `WorkflowDefinition`/in-memory bookkeeping. A fresh
/// engine per call (the pattern `src/workflow.rs`'s JSON pipeline uses)
/// would work for isolated JSON runs but would defeat `recover()`'s
/// "definition must already be registered on this engine" requirement here.
pub(crate) fn engine() -> &'static WorkflowEngine<WorkItemData, SqliteStore<WorkItemData>> {
    static ENGINE: std::sync::LazyLock<WorkflowEngine<WorkItemData, SqliteStore<WorkItemData>>> =
        std::sync::LazyLock::new(|| {
            let store = SqliteStore::open_file(&crate::workflow::default_db_path())
                .expect("open workflow store for work-item pipeline");
            WorkflowEngine::<WorkItemData, _>::with_store(store)
                .with_runtime_handle(crate::workflow::blocking_runtime_handle())
        });
    &ENGINE
}

/// Resumable entrypoint `execute_work` (Task 7) calls: (re-)registers the
/// per-dispatch pipeline definition, starts a fresh run or resumes an
/// in-flight/crashed one recorded on the item's `workflow_run_id` metadata,
/// and blocks synchronously (via `crate::workflow::blocking_runtime`,
/// reusing `src/workflow.rs`'s shared runtime) until the run reaches a
/// terminal state.
///
/// Re-registering on every call is safe: `WorkflowEngine::register_workflow`
/// (`crates/flare-workflow/src/engine.rs`) does a plain `HashMap::insert`
/// keyed by `WorkflowId` — it does NOT error on a duplicate id, it silently
/// overwrites the previous `Arc<WorkflowDefinition>`. Runs already in flight
/// hold their own `Arc` clone captured at `start_workflow` time, so they are
/// unaffected by a later overwrite. The one caveat (out of scope for this
/// task): `recover()`'s boot-time replay looks up whatever definition is
/// CURRENTLY registered for `WORKFLOW_ID`, so if two different items'
/// dispatches interleave around a crash, recovery could in principle replay
/// against the wrong item's prompts — Task 8 owns `recover()` wiring and
/// should account for this.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_or_resume(
    mcp: std::sync::Arc<AgentflareMcp>,
    item: &agentflare_backend::item::Item,
    agent: agent_registry::Agent,
    item_description: String,
    plan_doc: Option<String>,
    notify_recipient: Option<String>,
    timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
    extra_args: Vec<String>,
) -> Result<(), String> {
    run_or_resume_with_sender(
        mcp,
        item,
        agent,
        item_description,
        plan_doc,
        notify_recipient,
        real_agent_send_hook(timeout, idle_timeout, extra_args),
    )
}

/// Test seam: same resumable entrypoint, an injected `SendMessage` instead
/// of the real headless agent hook (mirrors every other step in this file).
/// `pub(crate)`: reused by `cli::work`'s own `execute_work` integration
/// test, a sibling module that needs to inject a mock sender the same way
/// this file's own tests do.
pub(crate) fn run_or_resume_with_sender(
    mcp: std::sync::Arc<AgentflareMcp>,
    item: &agentflare_backend::item::Item,
    agent: agent_registry::Agent,
    item_description: String,
    plan_doc: Option<String>,
    notify_recipient: Option<String>,
    send: flare_workflow::json::SendMessage,
) -> Result<(), String> {
    // Frames the description as untrusted content (BEGIN/END EXTERNAL
    // CONTENT markers) when the item came from an external GitHub issue —
    // see `wrap_if_external`'s own doc comment. Wrapped here, once, before
    // it's parsed into tasks, so every downstream implementer/reviewer/judge
    // prompt built from those tasks inherits the framing.
    let item_description = crate::cli::work::wrap_if_external(item, &item_description);

    let existing_metadata: serde_json::Value = serde_json::from_str(&item.metadata)
        .unwrap_or(serde_json::Value::Object(Default::default()));
    let existing_run_id = existing_metadata["workflow_run_id"]
        .as_str()
        .and_then(|s| flare_workflow::WorkflowRunId::from_str(s).ok());

    // Computed once, up front, from the same inputs passed to
    // `build_work_item_pipeline_with_sender` below — seeds `WorkItemData::tasks`
    // on both `start_workflow` call sites so `sdd_loop` (Task 5) has a task
    // list to work through instead of completing immediately on an empty one.
    let tasks = load_or_synthesize_tasks(&item_description, plan_doc.as_deref());

    let eng = engine();
    // Captured on the caller thread (still inside `with_owner_override` for
    // in-process dispatch, or on the env-derived identity for the CLI
    // subprocess path) BEFORE the workflow runs on `WORKFLOW_RT`'s worker
    // threads — `finalize` executes there, where the thread-local override
    // is absent, so it needs the owner passed down explicitly rather than
    // re-resolving `owner_id()` itself.
    let owner = crate::claims::owner_id();
    let definition = build_work_item_pipeline_with_sender(
        agent,
        item_description,
        plan_doc,
        mcp.clone(),
        item.id.clone(),
        owner,
        notify_recipient,
        send,
    );
    eng.register_workflow(definition)
        .map_err(|e| e.to_string())?;

    crate::workflow::blocking_runtime().block_on(async move {
        let run_id = match existing_run_id {
            Some(run_id) => {
                let state = eng.get_status(run_id).await.map_err(|e| e.to_string())?;
                if matches!(
                    state.status,
                    WorkflowStatus::Completed | WorkflowStatus::Failed | WorkflowStatus::Cancelled
                ) {
                    // Terminal — this is a genuine re-dispatch (e.g. a
                    // fresh self-repair pass), not a crash resume. Start
                    // over with a new run.
                    let new_run_id = eng
                        .start_workflow(
                            WorkflowId::new(WORKFLOW_ID),
                            WorkItemData {
                                tasks: tasks.clone(),
                                ..Default::default()
                            },
                            String::new(),
                        )
                        .await
                        .map_err(|e| e.to_string())?;
                    persist_run_id(&mcp, &item.id, &existing_metadata, new_run_id)?;
                    new_run_id
                } else {
                    // Non-terminal: either already resumed by the
                    // boot-time `recover()` sweep (Task 8) or genuinely
                    // still running in this same live process. Either
                    // way, do NOT start a second run against it — just
                    // await this one.
                    run_id
                }
            }
            None => {
                let new_run_id = eng
                    .start_workflow(
                        WorkflowId::new(WORKFLOW_ID),
                        WorkItemData {
                            tasks: tasks.clone(),
                            ..Default::default()
                        },
                        String::new(),
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                persist_run_id(&mcp, &item.id, &existing_metadata, new_run_id)?;
                new_run_id
            }
        };

        // The claim lease's TTL (30 min default, `crate::claims::ttl_secs`)
        // is far shorter than a work-item job is allowed to run (~6h05m,
        // `WORK_JOB_TIMEOUT_SECS`), and the SDD loop below has no heartbeat
        // of its own — without one here, a run spanning multiple tasks/fix
        // rounds would let the lease go stale mid-flight, letting another
        // sweep reclaim the item while this run is still actively working
        // it. Throttled well inside the TTL so it's negligible DB load
        // against this loop's 200ms poll cadence.
        let mut last_heartbeat = std::time::Instant::now();
        const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(300);

        loop {
            let state = eng.get_status(run_id).await.map_err(|e| e.to_string())?;
            match state.status {
                WorkflowStatus::Completed => return Ok(()),
                WorkflowStatus::Failed | WorkflowStatus::Cancelled => {
                    return Err(state
                        .error
                        .unwrap_or_else(|| "workflow run failed".to_string()));
                }
                _ => {
                    if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
                        let _ = mcp.item_heartbeat(ItemRequest {
                            action: "heartbeat".into(),
                            id: Some(item.id.clone()),
                            ..Default::default()
                        });
                        last_heartbeat = std::time::Instant::now();
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
            }
        }
    })
}

/// Merge `workflow_run_id` into the item's existing metadata JSON and save
/// it via `item_update` — how a fresh/re-dispatched run's id gets recorded
/// so `run_or_resume`'s next call (or a boot-time `recover()`) can find it.
fn persist_run_id(
    mcp: &AgentflareMcp,
    item_id: &str,
    existing_metadata: &serde_json::Value,
    run_id: flare_workflow::WorkflowRunId,
) -> Result<(), String> {
    let mut merged = existing_metadata.clone();
    merged["workflow_run_id"] = serde_json::Value::String(run_id.to_string());
    mcp.item_update(ItemRequest {
        action: "update".into(),
        id: Some(item_id.to_string()),
        metadata: Some(merged),
        ..Default::default()
    })
    .map(|_| ())
    .map_err(|e| e.message.to_string())
}

/// Parses `### Task N: <title>` headings (the convention this codebase's
/// own plans already use — see docs on item #110) into a task list; falls
/// back to a single synthesized task from the item's own description when
/// no plan doc is attached or it contains no recognizable task headings.
pub(crate) fn load_or_synthesize_tasks(
    item_description: &str,
    plan_doc: Option<&str>,
) -> Vec<SddTask> {
    if let Some(doc) = plan_doc.filter(|d| !d.trim().is_empty()) {
        let tasks = parse_task_headings(doc);
        if !tasks.is_empty() {
            return tasks;
        }
    }
    vec![SddTask {
        id: 0,
        title: "Item work".to_string(),
        body: item_description.to_string(),
        model_tier: None,
    }]
}

fn parse_task_headings(doc: &str) -> Vec<SddTask> {
    let mut tasks = Vec::new();
    let mut current: Option<(String, String)> = None;

    for line in doc.lines() {
        if let Some(title) = line.strip_prefix("### Task ").and_then(|rest| {
            let (_num, title) = rest.split_once(':')?;
            Some(title.trim().to_string())
        }) {
            if let Some((title, body)) = current.take() {
                tasks.push(SddTask {
                    id: tasks.len(),
                    title,
                    body: body.trim().to_string(),
                    model_tier: None,
                });
            }
            current = Some((title, String::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    if let Some((title, body)) = current {
        tasks.push(SddTask {
            id: tasks.len(),
            title,
            body: body.trim().to_string(),
            model_tier: None,
        });
    }
    tasks
}

/// Builds the prompt for the implementer role: given a task, it must implement
/// it. If `fix_context` is provided (a prior reviewer's findings), the prompt
/// instructs them to address those issues.
pub(crate) fn build_implementer_prompt(task: &SddTask, fix_context: Option<&str>) -> String {
    let mut prompt = format!(
        "You are implementing one task from a larger plan.\n\nTask: {}\n\n{}\n",
        task.title, task.body
    );
    if let Some(ctx) = fix_context {
        prompt.push_str(&format!(
            "\nA reviewer found issues with your prior attempt:\n{ctx}\n\nAddress them, re-run any tests you touched, and reply with your status.\n"
        ));
    }
    prompt.push_str("\nReply with a short status: what you did, tests run, and any concerns.\n");
    prompt
}

/// Builds the prompt for the task reviewer role: given a task and the
/// implementer's report, review it for spec compliance and code quality.
pub(crate) fn build_task_reviewer_prompt(task: &SddTask, implementer_report: &str) -> String {
    format!(
        "Review this task's implementation for spec compliance and code quality.\n\nTask: {}\n{}\n\nImplementer's report:\n{implementer_report}\n\nReply REVIEW_APPROVED if both spec and quality pass, or REVIEW_ISSUES: followed by a bulleted list of findings.\n",
        task.title, task.body
    )
}

/// Builds the prompt for the re-reviewer role: given a task, the original
/// findings, and a fix report, re-review only those specific findings.
pub(crate) fn build_re_reviewer_prompt(task: &SddTask, findings: &str, fix_report: &str) -> String {
    format!(
        "Re-review a fix for this task's findings only — do not look for new issues.\n\nTask: {}\n\nOriginal findings:\n{findings}\n\nFix report:\n{fix_report}\n\nReply REVIEW_APPROVED if every finding is addressed, or REVIEW_ISSUES: followed by what remains.\n",
        task.title
    )
}

/// Builds the prompt for the judge: given the task list, current task index,
/// ledger history, and the latest role reply, the judge decides what happens next.
pub(crate) fn build_judge_prompt(
    tasks: &[SddTask],
    current_task_index: usize,
    ledger: &[String],
    role_reply: &str,
) -> String {
    let task_list: String = tasks
        .iter()
        .enumerate()
        .map(|(i, t)| {
            format!(
                "{}. {}{}\n",
                i,
                t.title,
                if i == current_task_index {
                    " <- current"
                } else {
                    ""
                }
            )
        })
        .collect();
    let ledger_text: String = ledger.join("\n");
    format!(
        "You are the judge for an autonomous multi-task execution pipeline.\n\nPlan:\n{task_list}\n\nLedger so far:\n{ledger_text}\n\nLatest role reply:\n{role_reply}\n\nDecide what happens next. Reply with ONE JSON object and nothing else, matching exactly:\n{{\"action\": \"continue_task|fix_round|escalate|park_finding|rule_and_continue|insert_task|skip_task|advance_task|complete_pipeline\", \"rationale\": \"...\", \"ledger_line\": \"...\", \"task_model_tier\": \"mechanical|integration|architecture|null\"}}\n"
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
            ..Default::default()
        };
        let json = serde_json::to_string(&data).unwrap();
        let back: WorkItemData = serde_json::from_str(&json).unwrap();
        assert_eq!(back.reply_text, "did the thing");
        assert_eq!(
            back.pr_url.as_deref(),
            Some("https://github.com/x/y/pull/1")
        );
    }

    use flare_workflow::store::InMemoryStore;
    use flare_workflow::{WorkflowDefinition, WorkflowEngine, WorkflowId};
    use std::sync::Arc;

    /// Wires a bare local repo up as `origin` and pushes to it — same
    /// pattern `item_pr_failure_tests.rs`'s own fixture uses. Remotes are
    /// repo-wide (`.git/config`), so adding one to `repo_root` makes it
    /// visible from any of that repo's worktrees, including the item's own
    /// claimed worktree these tests dispatch into. Needed because
    /// `mcp_with_claimed_item`/`claim_harness` deliberately set up no
    /// `origin` at all, and `item_done` hard-fails (#482) when a real
    /// commit's push fails.
    fn add_origin_remote(repo_root: &std::path::Path) {
        let origin_dir = tempfile::tempdir().unwrap();
        let run = |dir: &std::path::Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(origin_dir.path(), &["init", "--bare", "-b", "master"]);
        run(
            repo_root,
            &[
                "remote",
                "add",
                "origin",
                origin_dir.path().to_str().unwrap(),
            ],
        );
        run(repo_root, &["push", "origin", "master"]);
        std::mem::forget(origin_dir);
    }

    #[tokio::test]
    #[ignore = "item_done hard-fails (#482) unless push succeeds AND a PR is \
                created; push_and_open_pr can't recognize a local bare repo \
                as GitHub, and this codebase has no mock GitHub client — \
                needs a real GitHub remote + credentials to reach Completed"]
    async fn finalize_step_calls_item_done_on_success() {
        let (mcp, _backend_tmp, repo_tmp, item_id, project_id, worktree_path) =
            crate::mcp_server::tests::mcp_with_claimed_item("Finalize test item");
        add_origin_remote(repo_tmp.path());
        // Something real to commit — otherwise `item_done` sees a
        // never-diverged branch and treats it as a no-op ("unchanged")
        // rather than a completion.
        std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();
        let mcp = Arc::new(mcp);

        let data = WorkItemData {
            reply_text: "implemented the thing".into(),
            ..Default::default()
        };
        let step = build_finalize_step(
            mcp.clone(),
            item_id.clone(),
            None,
            crate::claims::owner_id(),
        );
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

    // requires a real headless agent binary; run manually / in an
    // environment with one installed — the mock-sender variant right below
    // covers the same metadata-persistence assertion unconditionally.
    //
    // Deliberately a plain `#[test]` (not `#[tokio::test]`): `run_or_resume`
    // blocks synchronously via `crate::workflow::blocking_runtime().block_on`
    // on a *separate* runtime (`WORKFLOW_RT`); calling it from inside an
    // already-running tokio runtime (as an async test's own executor would
    // be) panics with "Cannot start a runtime from within a runtime" — the
    // same reason `src/workflow.rs`'s own `run_workflow_json`-driving tests
    // are plain `#[test]`s too.
    #[test]
    #[ignore]
    fn run_or_resume_persists_run_id_and_resume_skips_completed_coder_step() {
        let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, worktree_path) =
            crate::mcp_server::tests::mcp_with_claimed_item("Run-or-resume real-agent test item");
        std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();
        let mcp = Arc::new(mcp);
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
            .unwrap()
            .unwrap();

        // See the mock-sender variant below for why `engine()`'s db path
        // is isolated under a temp HOME.
        let result = crate::paths::test_support::with_temp_home(|| {
            run_or_resume(
                mcp.clone(),
                &item,
                agent_registry::Agent::ClaudeCode,
                "implement it".to_string(),
                None,
                None,
                std::time::Duration::from_secs(600),
                std::time::Duration::from_secs(300),
                Vec::new(),
            )
        });
        assert!(result.is_ok());

        let updated = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
            .unwrap()
            .unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&updated.metadata).unwrap();
        assert!(metadata["workflow_run_id"].as_str().is_some());
    }

    /// Mock-sender counterpart of the real-agent test above: drives
    /// `run_or_resume_with_sender` with a mock `SendMessage` that answers
    /// `sdd_loop`'s implementer and judge roles (distinguished by prompt
    /// content, same as `sdd_test_support::mock_send`'s callers), so it runs
    /// unconditionally in CI. Unlike `finalize_step_calls_item_done_on_success`
    /// this doesn't need a real GitHub PR to assert anything -- it only
    /// checks that `workflow_run_id` was persisted before `finalize`'s
    /// `item_done` call hard-fails on the missing PR (#482).
    #[test]
    fn run_or_resume_with_sender_persists_run_id_on_success() {
        let (mcp, _backend_tmp, repo_tmp, item_id, _project_id, worktree_path) =
            crate::mcp_server::tests::mcp_with_claimed_item("Run-or-resume mock-sender test item");
        add_origin_remote(repo_tmp.path());
        // Something real to commit — otherwise `finalize`'s `item_done` call
        // sees a never-diverged branch and treats it as a no-op rather than
        // a completion (see `finalize_step_calls_item_done_on_success`).
        std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();
        let mcp = Arc::new(mcp);
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
            .unwrap()
            .unwrap();

        let send: flare_workflow::json::SendMessage = Arc::new(
            move |inv: flare_workflow::json::StepInvocation| {
                let prompt = inv.prompt;
                Box::pin(async move {
                    if prompt.contains("You are the judge") {
                        Ok((
                            r#"{"action":"complete_pipeline","rationale":"done","ledger_line":"Task 0: complete","task_model_tier":null}"#
                                .to_string(),
                            1u64,
                            0u64,
                        ))
                    } else {
                        Ok(("DONE: did the work".to_string(), 1u64, 0u64))
                    }
                })
            },
        );

        // `engine()` is a process-lifetime singleton keyed by
        // `crate::workflow::default_db_path()` (~/.agentflare/workflows.db)
        // — isolate it under a temp HOME for this call so the test neither
        // depends on nor pollutes the real user state dir (and works in
        // sandboxes where `$HOME` is read-only).
        let result = crate::paths::test_support::with_temp_home(|| {
            run_or_resume_with_sender(
                mcp.clone(),
                &item,
                agent_registry::Agent::ClaudeCode,
                "implement it".to_string(),
                None,
                None,
                send,
            )
        });
        // `add_origin_remote` gives `git push` a real target but not a real
        // GitHub remote, so `finalize`'s `item_done` call correctly
        // hard-fails on the missing PR (item #109 / PR #482) -- same
        // reasoning as `finalize_step_calls_item_done_on_success` right
        // above, which needs `#[ignore]` for the same root cause since it
        // asserts completion rather than just metadata persistence. This
        // test only cares that `workflow_run_id` was persisted before that
        // failure, which happens well before `finalize` runs.
        assert!(result.is_err(), "{result:?}");

        let updated = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
            .unwrap()
            .unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&updated.metadata).unwrap();
        assert!(metadata["workflow_run_id"].as_str().is_some());
    }
}

#[cfg(test)]
mod pipeline_assembly_tests {
    use super::*;

    #[test]
    fn sdd_pipeline_has_two_steps_with_correct_dependency() {
        let send: flare_workflow::json::SendMessage =
            std::sync::Arc::new(|_: flare_workflow::json::StepInvocation| {
                Box::pin(async { Ok((String::new(), 0, 0)) })
            });
        let pipeline = build_work_item_pipeline_with_sender(
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
}

#[cfg(test)]
mod sdd_data_tests {
    use super::*;

    #[test]
    fn work_item_data_roundtrips_sdd_fields() {
        let data = WorkItemData {
            tasks: vec![SddTask {
                id: 0,
                title: "Add config flag".to_string(),
                body: "Add --verbose flag to CLI".to_string(),
                model_tier: Some(TaskModelTier::Mechanical),
            }],
            current_task_index: 0,
            fix_round: 0,
            ledger: vec!["Task 0: dispatched".to_string()],
            last_report: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&data).expect("serialize");
        let back: WorkItemData = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.tasks.len(), 1);
        assert_eq!(back.tasks[0].title, "Add config flag");
        assert_eq!(back.current_task_index, 0);
        assert_eq!(back.ledger, vec!["Task 0: dispatched".to_string()]);
    }
}

#[cfg(test)]
mod task_sourcing_tests {
    use super::load_or_synthesize_tasks;

    #[test]
    fn synthesizes_single_task_when_no_plan_doc() {
        let tasks = load_or_synthesize_tasks("Fix the null pointer in parser.rs", None);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, 0);
        assert_eq!(tasks[0].body, "Fix the null pointer in parser.rs");
    }

    #[test]
    fn parses_task_list_from_plan_doc_headings() {
        let plan_doc = "\
# Some Plan

### Task 1: Add validation

Add input validation to the handler.

### Task 2: Add tests

Add unit tests for the validation.
";
        let tasks = load_or_synthesize_tasks("ignored when plan_doc present", Some(plan_doc));
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].title, "Add validation");
        assert_eq!(tasks[1].title, "Add tests");
        assert!(tasks[1].body.contains("unit tests"));
    }

    #[test]
    fn empty_plan_doc_falls_back_to_synthesized_task() {
        let tasks = load_or_synthesize_tasks("Bump dependency version", Some(""));
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].body, "Bump dependency version");
    }
}

#[cfg(test)]
mod prompt_builder_tests {
    use super::*;

    fn sample_task() -> SddTask {
        SddTask {
            id: 0,
            title: "Add flag".to_string(),
            body: "Add --verbose".to_string(),
            model_tier: None,
        }
    }

    #[test]
    fn implementer_prompt_includes_task_body() {
        let prompt = build_implementer_prompt(&sample_task(), None);
        assert!(prompt.contains("Add --verbose"));
    }

    #[test]
    fn implementer_prompt_includes_fix_context_when_present() {
        let prompt = build_implementer_prompt(&sample_task(), Some("Reviewer found: missing test"));
        assert!(prompt.contains("Reviewer found: missing test"));
    }

    #[test]
    fn judge_prompt_instructs_json_only_output() {
        let prompt = build_judge_prompt(&[sample_task()], 0, &[], "DONE: implemented flag");
        assert!(prompt.contains("JSON"));
        assert!(prompt.contains("DONE: implemented flag"));
    }

    #[test]
    fn judge_prompt_includes_ledger_history() {
        let ledger = vec!["Task 0: fix round 1/5 (1 addressed)".to_string()];
        let prompt = build_judge_prompt(&[sample_task()], 0, &ledger, "REVIEW_APPROVED");
        assert!(prompt.contains("fix round 1/5"));
    }

    #[test]
    fn judge_prompt_formats_multiple_tasks_on_separate_lines() {
        let tasks = vec![
            SddTask {
                id: 0,
                title: "Add flag".to_string(),
                body: "Add --verbose".to_string(),
                model_tier: None,
            },
            SddTask {
                id: 1,
                title: "Fix bug".to_string(),
                body: "Fix null pointer".to_string(),
                model_tier: None,
            },
            SddTask {
                id: 2,
                title: "Add docs".to_string(),
                body: "Document the flag".to_string(),
                model_tier: None,
            },
        ];
        let prompt = build_judge_prompt(&tasks, 1, &[], "Test role reply");

        // Split the prompt by newlines and verify each task appears on its own line
        let lines: Vec<&str> = prompt.lines().collect();

        // Find the "Plan:" section and verify tasks are listed with proper line breaks
        let task_lines: Vec<&str> = lines
            .iter()
            .filter(|line| {
                line.contains("Add flag") || line.contains("Fix bug") || line.contains("Add docs")
            })
            .copied()
            .collect();

        // All three task titles should appear as separate lines (not concatenated)
        assert_eq!(
            task_lines.len(),
            3,
            "Expected 3 separate lines for 3 tasks, got: {:?}",
            task_lines
        );
        assert!(task_lines[0].contains("Add flag"));
        assert!(task_lines[1].contains("Fix bug") && task_lines[1].contains("<- current"));
        assert!(task_lines[2].contains("Add docs"));
    }
}

#[cfg(test)]
mod judge_decision_tests {
    use super::*;

    #[test]
    fn parses_valid_decision() {
        let reply = r#"{"action":"advance_task","rationale":"spec met","ledger_line":"Task 0: complete","task_model_tier":null}"#;
        let decision = parse_judge_decision(reply).expect("valid JSON parses");
        assert_eq!(decision.action, JudgeAction::AdvanceTask);
        assert_eq!(decision.ledger_line, "Task 0: complete");
    }

    #[test]
    fn parses_decision_wrapped_in_prose_by_stripping_to_the_json_object() {
        // Agents sometimes wrap JSON in a sentence despite instructions;
        // strip to the first {...} span before parsing.
        let reply = "Here is my decision:\n{\"action\":\"complete_pipeline\",\"rationale\":\"all tasks done\",\"ledger_line\":\"Pipeline: complete\",\"task_model_tier\":null}\nDone.";
        let decision = parse_judge_decision(reply).expect("parses after stripping");
        assert_eq!(decision.action, JudgeAction::CompletePipeline);
    }

    #[test]
    fn rejects_malformed_json() {
        let err = parse_judge_decision("not json at all").unwrap_err();
        assert!(matches!(err, JudgeParseError::InvalidJson(_)));
    }

    #[test]
    fn rejects_unknown_action_value() {
        let reply = r#"{"action":"do_a_barrel_roll","rationale":"x","ledger_line":"x","task_model_tier":null}"#;
        let err = parse_judge_decision(reply).unwrap_err();
        assert!(matches!(err, JudgeParseError::InvalidJson(_)));
    }

    #[test]
    fn parses_decision_from_a_json_fenced_code_block() {
        let reply = "Here's my decision:\n```json\n{\"action\":\"advance_task\",\"rationale\":\"spec met\",\"ledger_line\":\"Task 0: complete\",\"task_model_tier\":null}\n```\nThanks.";
        let decision = parse_judge_decision(reply).expect("parses fenced block");
        assert_eq!(decision.action, JudgeAction::AdvanceTask);
    }

    #[test]
    fn parses_decision_from_a_bare_fenced_code_block_with_no_language_tag() {
        let reply = "```\n{\"action\":\"skip_task\",\"rationale\":\"x\",\"ledger_line\":\"x\",\"task_model_tier\":null}\n```";
        let decision = parse_judge_decision(reply).expect("parses bare fenced block");
        assert_eq!(decision.action, JudgeAction::SkipTask);
    }

    #[test]
    fn extracts_only_the_first_object_when_trailing_prose_has_its_own_unrelated_braces() {
        // A naive first-`{`-to-last-`}` span would run from the real
        // object's opening brace all the way through the unrelated `{cfg}`
        // in the trailing sentence, producing invalid combined JSON.
        let reply = "{\"action\":\"advance_task\",\"rationale\":\"spec met\",\"ledger_line\":\"Task 0: complete\",\"task_model_tier\":null}\nNote: this respects the {cfg} override.";
        let decision =
            parse_judge_decision(reply).expect("parses despite trailing unrelated braces");
        assert_eq!(decision.action, JudgeAction::AdvanceTask);
    }

    #[test]
    fn a_brace_inside_a_json_string_value_does_not_confuse_balance_tracking() {
        let reply = r#"{"action":"advance_task","rationale":"uses a {placeholder} pattern","ledger_line":"x","task_model_tier":null}"#;
        let decision = parse_judge_decision(reply).expect("brace inside string value is inert");
        assert_eq!(decision.action, JudgeAction::AdvanceTask);
    }

    #[test]
    fn rejects_syntactically_valid_json_missing_a_required_field() {
        // Reproduction of the live 2026-08-15 production failure (item
        // #478): a well-formed JSON object that's simply missing `action`
        // must still be a genuine, retried parse failure -- not silently
        // defaulted.
        let reply = r#"{"rationale":"x","ledger_line":"x","task_model_tier":null}"#;
        let err = parse_judge_decision(reply).unwrap_err();
        assert!(matches!(err, JudgeParseError::InvalidJson(msg) if msg.contains("action")));
    }

    #[test]
    fn rejects_an_empty_reply() {
        let err = parse_judge_decision("").unwrap_err();
        assert!(matches!(err, JudgeParseError::InvalidJson(_)));
    }
}

/// Shared fixtures for `sdd_loop_tests` (this task) and later plan tasks'
/// test modules (Task 6's `cap_tests`, Tasks 11/12) that need the same
/// mocked `send` and a minimal single-task `WorkItemData` — a sibling
/// `#[cfg(test)] mod` can't reach into another sibling module's private
/// items, so these live in their own module and get pulled in via
/// `use super::sdd_test_support::*;`.
#[cfg(test)]
mod sdd_test_support {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    /// Records every `(agent_name, prompt)` call and returns queued replies
    /// in order.
    #[allow(clippy::type_complexity)]
    pub(crate) fn mock_send(
        replies: Vec<&'static str>,
    ) -> (
        flare_workflow::json::SendMessage,
        Arc<Mutex<Vec<(String, String)>>>,
    ) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let queue = Arc::new(Mutex::new(replies.into_iter().collect::<VecDeque<_>>()));
        let calls_clone = calls.clone();
        let send: flare_workflow::json::SendMessage =
            Arc::new(move |inv: flare_workflow::json::StepInvocation| {
                calls_clone
                    .lock()
                    .unwrap()
                    .push((inv.agent.clone(), inv.prompt.clone()));
                let reply = queue.lock().unwrap().pop_front().unwrap_or("").to_string();
                Box::pin(async move { Ok((reply, 10u64, 10u64)) })
            });
        (send, calls)
    }

    pub(crate) fn one_task_data() -> WorkItemData {
        WorkItemData {
            tasks: vec![SddTask {
                id: 0,
                title: "Add flag".to_string(),
                body: "Add --verbose".to_string(),
                model_tier: None,
            }],
            ..Default::default()
        }
    }

    /// `build_sdd_loop_step` with the fixed agent names `sdd_loop_tests` uses.
    pub(crate) fn sdd_step(
        send: flare_workflow::json::SendMessage,
    ) -> StepDefinition<WorkItemData> {
        build_sdd_loop_step(
            "implementer-agent".to_string(),
            "judge-agent".to_string(),
            send,
        )
    }
}

#[cfg(test)]
mod sdd_loop_tests {
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
}
#[cfg(test)]
mod cap_tests {
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
        let step = build_sdd_loop_step("a".to_string(), "b".to_string(), send);
        let mut ctx = WorkflowContext::new(Default::default(), d);
        assert!(matches!(
            step.executor.execute(&mut ctx).await.expect("x"),
            StepResult::Failure
        ));
    }
    #[tokio::test]
    async fn max_tasks_processed_bound_fails_the_step() {
        let (send, _) = mock_send(vec![]);
        let mut d = one_task_data();
        d.current_task_index = MAX_TASKS_PROCESSED;
        let step = build_sdd_loop_step("a".to_string(), "b".to_string(), send);
        let mut ctx = WorkflowContext::new(Default::default(), d);
        assert!(matches!(
            step.executor.execute(&mut ctx).await.expect("x"),
            StepResult::Failure
        ));
    }
}
