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
    /// Id of the item this run is working — persisted (not closed over at
    /// registration time) so a crash-resumed run's `finalize` targets the
    /// real item instead of an empty placeholder.
    ///
    /// `#[serde(default)]`: persisted `state_json` from runs started before
    /// this field existed has no `item_id` key — without a default,
    /// `SqliteStore::load` fails to deserialize those rows and `recover()`
    /// silently skips them as unreadable (same reasoning as `review_only`
    /// below, and true of every field added in this same change).
    #[serde(default)]
    pub item_id: String,
    /// Agent dispatched for the implementer/task-reviewer/re-reviewer roles
    /// in `sdd_loop` — persisted for the same crash-resume reason as `item_id`.
    #[serde(default)]
    pub agent_name: String,
    /// Agent dispatched for the judge role in `sdd_loop`.
    #[serde(default)]
    pub judge_agent_name: String,
    /// Recipient `finalize` notifies on hold/completion, if any.
    #[serde(default)]
    pub notify_recipient: Option<String>,
    /// `crate::claims::owner_id()` captured on the caller thread when the run
    /// was started — `finalize` runs on `WORKFLOW_RT`'s worker threads, where
    /// the thread-local owner override is absent, so it reads this instead of
    /// re-resolving `owner_id()` itself.
    #[serde(default)]
    pub owner: String,
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
    /// Every analyst report produced by a review-only run (item #507).
    /// `last_report`/`review_issues` are cleared on `advance_task`/
    /// `skip_task` (see the judge-decision handler), so for a single-task
    /// run that clears them on the very iteration it completes, `finalize`
    /// would otherwise see both as `None` and post "No findings reported."
    /// even though the analyst produced real output. `finalize` reads this
    /// instead when non-empty.
    #[serde(default)]
    pub review_findings: Vec<String>,
    /// Set from `detect_review_only` at dispatch time (item #507) — routes
    /// `sdd_loop`'s role prompts to analyze-and-report phrasing instead of
    /// implement-and-commit, and routes `finalize` to post the findings as a
    /// comment instead of running `item_done`/PR flow. Without this, a
    /// handoff explicitly asking for a review only (see item #502) still
    /// gets silently converted into an implementation attempt.
    ///
    /// `#[serde(default)]`: persisted `state_json` from runs started before
    /// this field existed has no `review_only` key — without a default,
    /// `SqliteStore::load` fails to deserialize those rows and `recover()`
    /// silently skips them as unreadable.
    #[serde(default)]
    pub review_only: bool,
    /// Provider session id last observed for each agent name dispatched in
    /// this run (implementer and judge/reviewer are usually different
    /// agents and get independent entries). Used to pass `--resume <id>` on
    /// that agent's next turn instead of respawning a cold, full-context
    /// session.
    #[serde(default)]
    pub agent_sessions: std::collections::HashMap<String, String>,
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

/// Smuggles a captured provider session id back through the plain-string
/// reply channel `flare_workflow::json::SendMessage` returns, the same way
/// REVIEW_ISSUES_MARKER/REVIEW_APPROVED_MARKER above smuggle control
/// signals — chosen specifically so SendMessage's type never needs to
/// widen. Always stripped before a reply is stored in ctx.data or used to
/// build any further prompt — it never reaches a judge prompt, a PR
/// comment, or a human.
const SESSION_MARKER: &str = "\u{0}AGENTFLARE_SESSION:";

fn encode_session(reply: &str, session_id: Option<&str>) -> String {
    match session_id {
        Some(id) => format!("{reply}{SESSION_MARKER}{id}"),
        None => reply.to_string(),
    }
}

fn strip_session_marker(reply: &str) -> (String, Option<String>) {
    match reply.split_once(SESSION_MARKER) {
        Some((clean, id)) => (clean.to_string(), Some(id.to_string())),
        None => (reply.to_string(), None),
    }
}

fn resume_args_for(
    agent_name: &str,
    sessions: &std::collections::HashMap<String, String>,
) -> Vec<String> {
    let Some(agent) = agent_registry::agent_by_name(agent_name) else {
        return Vec::new();
    };
    let Some(flag) = agent_registry::resume_arg(agent) else {
        return Vec::new();
    };
    match sessions.get(agent_name) {
        Some(session_id) => vec![flag.to_string(), session_id.clone()],
        None => Vec::new(),
    }
}

/// True when a `send` failure means the resumed provider session no longer
/// exists — e.g. it was born under a daemon process that crashed/restarted
/// mid-run (item #159) — rather than a transient failure worth retrying
/// as-is. Matches Claude Code's `claude --resume <dead-id>` stderr; other
/// `resume_arg` agents (Cursor) are expected to fail the same recognizable
/// way, but none has been observed yet to confirm the exact text.
fn is_stale_session_error(message: &str) -> bool {
    message.to_lowercase().contains("no conversation found")
}

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
/// closure over an injected `send`, same test seam pattern. `agent_name`/
/// `judge_agent_name` are read from `ctx.data` at execution time (not
/// closed over here) so a run resumed by `engine().recover()` after a crash
/// dispatches against the real agent identity the crashed run persisted,
/// not whatever happened to be registered at boot.
pub(crate) fn build_sdd_loop_step(
    send: flare_workflow::json::SendMessage,
) -> StepDefinition<WorkItemData> {
    let executor = std::sync::Arc::new(FunctionStep::new(
        move |ctx: &mut WorkflowContext<WorkItemData>| {
            let send = send.clone();
            Box::pin(async move {
                if ctx.data.current_task_index >= MAX_TASKS_PROCESSED {
                    return Ok(StepResult::Failed(format!(
                        "current_task_index {} reached MAX_TASKS_PROCESSED ({})",
                        ctx.data.current_task_index, MAX_TASKS_PROCESSED
                    )));
                }
                if ctx.data.tasks.is_empty() || ctx.data.current_task_index >= ctx.data.tasks.len()
                {
                    ctx.output = SDD_PIPELINE_COMPLETE_MARKER.to_string();
                    return Ok(StepResult::Success);
                }

                // Read at execution time, not closed over at
                // step-registration time -- so a run resumed by
                // `engine().recover()` against a freshly-registered,
                // identity-less definition still dispatches the crashed
                // run's real agents. See `WorkItemData::item_id`'s doc
                // comment for the full rationale.
                let agent_name = ctx.data.agent_name.clone();
                let judge_agent_name = ctx.data.judge_agent_name.clone();
                if agent_name.is_empty() || judge_agent_name.is_empty() {
                    return Ok(StepResult::Failed(format!(
                        "sdd_loop missing agent identity: agent_name={agent_name:?} judge_agent_name={judge_agent_name:?}"
                    )));
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
                        // implementer to fix them (or, for a review-only
                        // task, the analyst to revise their findings).
                        let fix_context = ctx.data.review_issues.as_deref();
                        let prompt = if ctx.data.review_only {
                            build_review_analyst_prompt(&task, fix_context)
                        } else {
                            build_implementer_prompt(&task, fix_context)
                        };
                        (agent_name.clone(), prompt)
                    }
                } else if ctx.data.last_report.is_some() {
                    // No open issues; a report is pending review.
                    let report = ctx.data.last_report.clone().unwrap_or_default();
                    let prompt = if ctx.data.review_only {
                        build_review_of_analysis_prompt(&task, &report)
                    } else {
                        build_task_reviewer_prompt(&task, &report)
                    };
                    (judge_agent_name.clone(), prompt)
                } else {
                    // Fresh task, nothing dispatched yet.
                    let prompt = if ctx.data.review_only {
                        build_review_analyst_prompt(&task, None)
                    } else {
                        build_implementer_prompt(&task, None)
                    };
                    (agent_name.clone(), prompt)
                };

                let role_invocation = flare_workflow::json::StepInvocation {
                    args: resume_args_for(&role_agent, &ctx.data.agent_sessions),
                    ..flare_workflow::json::StepInvocation::simple(role_agent.clone(), role_prompt)
                };
                let (raw_role_reply, in_tok, out_tok) =
                    send(role_invocation).await.map_err(|message| {
                        // A dead resumed session would otherwise fail the
                        // same way on every one of this step's retry
                        // attempts (same session_id -> same `--resume`
                        // failure) until `RetryPolicy` gives up — clearing
                        // it here makes the very next attempt fall back to
                        // a fresh prompt instead.
                        if is_stale_session_error(&message) {
                            ctx.data.agent_sessions.remove(&role_agent);
                        }
                        WorkflowError::StepFailed {
                            step_id: StepId::new("sdd_loop"),
                            message,
                        }
                    })?;
                ctx.input_tokens += in_tok;
                ctx.output_tokens += out_tok;

                let (role_reply, role_session_id) = strip_session_marker(&raw_role_reply);
                if let Some(id) = role_session_id {
                    ctx.data
                        .agent_sessions
                        .insert(role_agent.clone(), id.clone());
                    if role_agent == agent_name {
                        ctx.data.session_id = Some(id);
                    }
                }

                if let Some(issues) = role_reply.strip_prefix(REVIEW_ISSUES_MARKER) {
                    ctx.data.review_issues = Some(issues.trim().to_string());
                    ctx.data.last_report = None;
                } else if role_reply.trim() == REVIEW_APPROVED_MARKER {
                    ctx.data.review_issues = None;
                } else {
                    ctx.data.last_report = Some(role_reply.clone());
                    if ctx.data.review_only {
                        ctx.data.review_findings.push(role_reply.clone());
                    }
                }

                // 2. Judge dispatch.
                let judge_prompt = build_judge_prompt(
                    &ctx.data.tasks,
                    ctx.data.current_task_index,
                    &ctx.data.ledger,
                    &role_reply,
                    ctx.data.review_only,
                );
                let judge_invocation = flare_workflow::json::StepInvocation {
                    args: resume_args_for(&judge_agent_name, &ctx.data.agent_sessions),
                    ..flare_workflow::json::StepInvocation::simple(
                        judge_agent_name.clone(),
                        judge_prompt,
                    )
                };
                let (raw_judge_reply, jin_tok, jout_tok) =
                    send(judge_invocation).await.map_err(|message| {
                        if is_stale_session_error(&message) {
                            ctx.data.agent_sessions.remove(&judge_agent_name);
                        }
                        WorkflowError::StepFailed {
                            step_id: StepId::new("sdd_loop"),
                            message,
                        }
                    })?;
                ctx.input_tokens += jin_tok;
                ctx.output_tokens += jout_tok;

                let (judge_reply, judge_session_id) = strip_session_marker(&raw_judge_reply);
                if let Some(id) = judge_session_id {
                    ctx.data.agent_sessions.insert(judge_agent_name.clone(), id);
                }

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
                            return Ok(StepResult::Failed(format!(
                                "task {} exceeded MAX_FIX_ROUNDS ({}) without converging",
                                task.id, MAX_FIX_ROUNDS
                            )));
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
                            return Ok(StepResult::Failed(format!(
                                "cannot insert task: tasks.len() {} reached MAX_TASKS_PROCESSED ({})",
                                ctx.data.tasks.len(),
                                MAX_TASKS_PROCESSED
                            )));
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
/// step. Four outcomes, checked in order:
///
/// 1. `ctx.data.hold_reason` set (Task 3's `coder` step detected an
///    `AGENTFLARE_HOLD:` signal) — release the claim and post an "on hold"
///    comment instead of calling `item_done`, same as `execute_work`'s hold
///    branch.
/// 2. `ctx.data.review_only` set (item #507 — the dispatched item asked for
///    analysis, not implementation) — release the claim and post the
///    accumulated findings as a comment instead of ever reaching
///    `item_done`/PR flow, regardless of `review_issues` state.
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
                let item_id = ctx.data.item_id.clone();
                let notify_recipient = ctx.data.notify_recipient.clone();
                let owner = ctx.data.owner.clone();
                crate::claims::with_owner_override(owner, || {
                    if let Some(reason) = ctx.data.hold_reason.clone() {
                        finalize_release_claim_best_effort(&mcp, &item_id, false);
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

                    if ctx.data.review_only {
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
                        finalize_release_claim_best_effort(&mcp, &item_id, false);
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
///
/// Takes no item/agent-specific arguments: `sdd_loop`/`finalize`'s executors
/// read that identity from `ctx.data` (`WorkItemData::item_id`/`agent_name`/
/// `judge_agent_name`/`notify_recipient`) at execution time instead, which is
/// what makes the same registered definition work for both a fresh dispatch
/// and a crash-resumed one — see `run_or_resume_with_sender` for where that
/// data gets seeded.
pub(crate) fn build_work_item_pipeline(
    mcp: std::sync::Arc<AgentflareMcp>,
    timeout: std::time::Duration,
    idle_timeout: std::time::Duration,
    extra_args: Vec<String>,
) -> flare_workflow::WorkflowDefinition<WorkItemData> {
    build_work_item_pipeline_with_sender(
        mcp,
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
        let mut all_args = extra_args.clone();
        all_args.extend(inv.args.clone());
        let flare_workflow::json::StepInvocation { agent, prompt, .. } = inv;
        Box::pin(async move {
            let agent_for_reply = agent.clone();
            let outcome = tokio::task::spawn_blocking(move || {
                crate::agent_launch::run_headless(
                    agent_registry::REGISTRY,
                    &agent,
                    &prompt,
                    timeout,
                    idle_timeout,
                    &all_args,
                    true,
                )
            })
            .await
            .map_err(|e| format!("agent task panicked: {e}"))?;
            match outcome {
                crate::agent_launch::HeadlessOutcome::Ok(reply) => Ok((
                    encode_session(
                        &crate::agent_launch::clean_agent_reply(&agent_for_reply, reply.text),
                        reply.session_id.as_deref(),
                    ),
                    0,
                    0,
                )),
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
/// pattern). No per-item identity here either — see
/// `build_work_item_pipeline`'s doc comment.
fn build_work_item_pipeline_with_sender(
    mcp: std::sync::Arc<AgentflareMcp>,
    send: flare_workflow::json::SendMessage,
) -> flare_workflow::WorkflowDefinition<WorkItemData> {
    let sdd_loop = build_sdd_loop_step(send);
    let finalize = build_finalize_step(mcp).depends_on(&["sdd_loop"]);

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
/// unaffected by a later overwrite. `recover()`'s boot-time replay looks up
/// whatever definition is CURRENTLY registered for `WORKFLOW_ID`, but that's
/// no longer a hazard if two different items' dispatches interleave around a
/// crash: `build_work_item_pipeline_with_sender` takes no item/agent-specific
/// arguments, every registered definition's `sdd_loop`/`finalize` steps read
/// that identity from the resumed run's own persisted `ctx.data` instead.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_or_resume(
    mcp: std::sync::Arc<AgentflareMcp>,
    item: &agentflare_backend::item::Item,
    implementer_agent: agent_registry::Agent,
    review_agent: agent_registry::Agent,
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
        implementer_agent,
        review_agent,
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_or_resume_with_sender(
    mcp: std::sync::Arc<AgentflareMcp>,
    item: &agentflare_backend::item::Item,
    implementer_agent: agent_registry::Agent,
    review_agent: agent_registry::Agent,
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

    // Computed once, up front — seeds `WorkItemData::tasks` on both
    // `start_workflow` call sites below so `sdd_loop` has a task list to
    // work through instead of completing immediately on an empty one.
    let tasks = load_or_synthesize_tasks(&item_description, plan_doc.as_deref());
    // Seeds `WorkItemData::review_only` (item #507) the same way — computed
    // once here so both `start_workflow` call sites below agree.
    let review_only = detect_review_only(&item_description, &existing_metadata);

    let agent_name = implementer_agent.as_str().to_string();
    let judge_agent_name = review_agent.as_str().to_string();
    // Captured on the caller thread (still inside `with_owner_override` for
    // in-process dispatch, or on the env-derived identity for the CLI
    // subprocess path) BEFORE the workflow runs on `WORKFLOW_RT`'s worker
    // threads — `finalize` executes there, where the thread-local override
    // is absent, so it's seeded into `WorkItemData::owner` below rather than
    // `finalize` re-resolving `owner_id()` itself.
    let owner = crate::claims::owner_id();
    let heartbeat_owner = owner.clone();

    let eng = engine();
    let definition = build_work_item_pipeline_with_sender(mcp.clone(), send);
    eng.register_workflow(definition)
        .map_err(|e| e.to_string())?;

    crate::workflow::blocking_runtime().block_on(async move {
        // Seeded onto every fresh `start_workflow` call below (not carried
        // by the definition itself, see `build_work_item_pipeline_with_sender`)
        // so `sdd_loop`/`finalize` can rebuild their own identity from
        // `ctx.data` at execution time — including on a crash-resumed run,
        // whose registration (the boot-time sweep in `dashboard/server.rs`)
        // never sees this item/agent at all.
        let fresh_data = || WorkItemData {
            item_id: item.id.clone(),
            agent_name: agent_name.clone(),
            judge_agent_name: judge_agent_name.clone(),
            owner: owner.clone(),
            notify_recipient: notify_recipient.clone(),
            tasks: tasks.clone(),
            review_only,
            ..Default::default()
        };
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
                        .start_workflow(WorkflowId::new(WORKFLOW_ID), fresh_data(), String::new())
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
                    .start_workflow(WorkflowId::new(WORKFLOW_ID), fresh_data(), String::new())
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
                        let _ = crate::claims::with_owner_override(heartbeat_owner.clone(), || {
                            mcp.item_heartbeat(ItemRequest {
                                action: "heartbeat".into(),
                                id: Some(item.id.clone()),
                                ..Default::default()
                            })
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
    // `existing_metadata` can be a non-object (e.g. a double-JSON-encoded
    // string, confirmed live on item #331) when the item's stored metadata
    // is corrupted -- `Value`'s `IndexMut` panics assigning a key into
    // anything that isn't already `Object`, so coerce defensively instead
    // of trusting the caller-supplied value's shape.
    let mut merged = existing_metadata
        .as_object()
        .cloned()
        .map(serde_json::Value::Object)
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
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

/// Decides whether a dispatched item is a no-code task (item #507, widened
/// by #156): `sdd_loop` should analyze/propose rather than implement, and
/// `finalize` should post a findings comment rather than running
/// `item_done`/PR flow. `metadata["task_type"]` of `"review"` or
/// `"design-spec"`/`"design_spec"` is the authoritative signal — set it at
/// handoff/dispatch time when the caller already knows the task type (e.g.
/// PM-mode's own task-type routing); nothing sets it in production yet, so
/// in practice the description-phrase fallback below is load-bearing.
///
/// The fallback matches the "review only" / "design-spec" framing a
/// human/agent handoff uses in prose when no structured field is set —
/// item #502's own handoff read "REVIEW ONLY — do not fix, do not push, do
/// not open a PR." with nothing else marking it as such, and the pipeline
/// implemented it anyway. Hyphens are normalized to spaces before matching
/// because the PM skill's own routing table (`.claude/skills/pm/SKILL.md`)
/// tells callers to write the hyphenated "review-only", which the original
/// space-separated-only check missed (item #156).
pub(crate) fn detect_review_only(item_description: &str, metadata: &serde_json::Value) -> bool {
    if matches!(
        metadata["task_type"].as_str(),
        Some("review") | Some("design-spec") | Some("design_spec")
    ) {
        return true;
    }
    let normalized = item_description.to_lowercase().replace('-', " ");
    if normalized.contains("review only") {
        return true;
    }
    // Word-boundary match on "design spec" so "design specification"/"design
    // specs" (ordinary implementation-task phrasing) doesn't false-positive
    // on the "spec" prefix.
    let words: Vec<&str> = normalized.split_whitespace().collect();
    words.windows(2).any(|pair| {
        pair[0].trim_matches(|c: char| !c.is_alphanumeric()) == "design"
            && pair[1].trim_matches(|c: char| !c.is_alphanumeric()) == "spec"
    })
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

/// Review-only counterpart of `build_implementer_prompt` (item #507): same
/// fix-round re-dispatch shape, but the role is constrained to analysis —
/// it must never write, edit, or commit code, or open a pull request.
pub(crate) fn build_review_analyst_prompt(task: &SddTask, fix_context: Option<&str>) -> String {
    let mut prompt = format!(
        "You are reviewing one task from a larger plan — analysis only. Do not write, edit, or commit any code, and do not open a pull request.\n\nTask: {}\n\n{}\n",
        task.title, task.body
    );
    if let Some(ctx) = fix_context {
        prompt.push_str(&format!(
            "\nA second reviewer flagged gaps in your prior analysis:\n{ctx}\n\nAddress them and reply with your updated findings.\n"
        ));
    }
    prompt.push_str(
        "\nReply with your findings: what you reviewed and any issues found (or none).\n",
    );
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

/// Review-only counterpart of `build_task_reviewer_prompt` (item #507):
/// checks the analyst's findings for completeness/accuracy instead of "spec
/// compliance and code quality" — there's no code for this second pass to
/// check, only the first pass's own analysis.
pub(crate) fn build_review_of_analysis_prompt(task: &SddTask, analyst_report: &str) -> String {
    format!(
        "Review this analysis for completeness and accuracy — is anything missing or wrong?\n\nTask: {}\n{}\n\nAnalyst's report:\n{analyst_report}\n\nReply REVIEW_APPROVED if the analysis is thorough and accurate, or REVIEW_ISSUES: followed by a bulleted list of gaps.\n",
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
    review_only: bool,
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
    let mode_note = if review_only {
        "This is a review-only task: no code should be written; the role's job is to analyze and report findings, not implement fixes.\n\n"
    } else {
        ""
    };
    format!(
        "You are the judge for an autonomous multi-task execution pipeline.\n\n{mode_note}Plan:\n{task_list}\n\nLedger so far:\n{ledger_text}\n\nLatest role reply:\n{role_reply}\n\nDecide what happens next. Reply with ONE JSON object and nothing else, matching exactly:\n{{\"action\": \"continue_task|fix_round|escalate|park_finding|rule_and_continue|insert_task|skip_task|advance_task|complete_pipeline\", \"rationale\": \"...\", \"ledger_line\": \"...\", \"task_model_tier\": \"mechanical|integration|architecture|null\"}}\n"
    )
}

#[cfg(test)]
mod cap_tests;
#[cfg(test)]
mod judge_decision_tests;
#[cfg(test)]
mod pipeline_assembly_tests;
#[cfg(test)]
mod prompt_builder_tests;
#[cfg(test)]
mod review_only_detection_tests;
#[cfg(test)]
mod sdd_data_tests;
#[cfg(test)]
mod sdd_loop_tests;
#[cfg(test)]
mod sdd_test_support;
#[cfg(test)]
mod task_sourcing_tests;
#[cfg(test)]
mod tests;
