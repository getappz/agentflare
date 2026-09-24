// Agent-exhaustion failover and stop-on-request handling for `execute_work`.
// Included from `work.rs` (keeps that file under the LOC gate).

/// Failure text a work-item pipeline returns when its run was **paused** on
/// request (as opposed to cancelled or reassigned). `execute_work` maps it
/// to a terminal, non-retried job whose item is neither counted toward the
/// dispatch-failure ceiling nor put back on `ready-for-work` -- resuming is
/// the pauser's job (re-add `ready-for-work`, or `item action=redispatch`).
/// Matched with `contains`, so it may be wrapped in more text.
pub(crate) const PAUSED_MESSAGE: &str = "paused: work item run paused on request";

/// Why a run stopped on purpose rather than failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopOnRequest {
    /// The job itself was cancelled (`agentflare_jobs::cancel`): item
    /// reassigned, or an operator cancelled/paused the job. The queue
    /// finishes such a job as `killed` itself.
    JobCancelled,
    /// The item's workflow run was cancelled (status `Cancelled`).
    RunCancelled,
    /// The pipeline returned [`PAUSED_MESSAGE`].
    Paused,
}

/// Classifies a pipeline error as a deliberate stop. `run_cancelled` is
/// whether the item's workflow run is in `Cancelled` state (see
/// [`run_cancelled_by_this_dispatch`]) -- a cancelled run otherwise surfaces only as
/// its (possibly empty) error text, indistinguishable from a failure.
pub(crate) fn stop_on_request(msg: &str, run_cancelled: bool) -> Option<StopOnRequest> {
    if msg.contains(PAUSED_MESSAGE) {
        Some(StopOnRequest::Paused)
    } else if msg.contains(agentflare_jobs::cancel::CANCELLED_MESSAGE) {
        Some(StopOnRequest::JobCancelled)
    } else if run_cancelled || msg.starts_with("Workflow cancelled") {
        Some(StopOnRequest::RunCancelled)
    } else {
        None
    }
}

/// The workflow run recorded on `item_id`'s `workflow_run_id` metadata, if
/// that run is `Cancelled`. `None` on any lookup failure.
fn cancelled_workflow_run(mcp: &AgentflareMcp, item_id: &str) -> Option<String> {
    let stored = mcp
        .with_backend_db(|conn| {
            let item = agentflare_backend::item::get(conn, item_id).ok()?;
            let meta: serde_json::Value = serde_json::from_str(&item.metadata).ok()?;
            meta["workflow_run_id"].as_str().map(str::to_string)
        })
        .ok()
        .flatten()?;
    let run_id = <flare_workflow::WorkflowRunId as std::str::FromStr>::from_str(&stored).ok()?;
    // On a fresh thread: blocking on the workflow runtime from a thread
    // that is itself inside an async context would panic.
    let cancelled = std::thread::spawn(move || {
        crate::workflow::blocking_runtime()
            .block_on(crate::work_item_pipeline::engine().get_status(run_id))
            .is_ok_and(|state| state.status == flare_workflow::WorkflowStatus::Cancelled)
    })
    .join()
    .unwrap_or(false);
    cancelled.then_some(stored)
}

/// Whether *this* dispatch's run was cancelled: the item's stored run is
/// `Cancelled` now (`after`) and it isn't the same run that was already
/// `Cancelled` before the dispatch started (`before`). A dispatch that finds
/// a cancelled run starts a fresh one but only persists the new id once
/// `start_workflow` returns -- so a setup error before that leaves the stale
/// cancelled id in place, which must not read as a cancel-on-request.
pub(crate) fn run_cancelled_by_this_dispatch(before: Option<&str>, after: Option<&str>) -> bool {
    after.is_some() && after != before
}

/// Ends a run cancelled or paused on request: drops this job's own lease
/// (owner-scoped, keeps the worktree for a resume), posts the neutral
/// `STOPPED_ON_REQUEST_MARKER` comment -- which both keeps the stop out of
/// the dispatch-failure ceiling and tells the terminal-job hook not to
/// re-arm `ready-for-work` -- and fails the job fatally so it isn't retried.
fn end_stopped_run(
    mcp: &AgentflareMcp,
    item_id: &str,
    stop: StopOnRequest,
    claim_guard: &mut ClaimGuard,
    log: &mut dyn std::io::Write,
) -> WorkOutcome {
    claim_guard.disarm();
    let owner = crate::claims::owner_id();
    let _ = mcp.with_backend_db(|conn| agentflare_backend::claim::release(conn, item_id, &owner));
    let what = match stop {
        StopOnRequest::Paused => "paused",
        _ => "cancelled",
    };
    let how = match stop {
        // Discovery skips `paused` items; only resume clears that label.
        StopOnRequest::Paused => "Run `agentflare item resume <item>` to continue it",
        _ => "Add `ready-for-work` (or `item action=redispatch`) to run it again",
    };
    let body = format!(
        "{}\n\nworkflow run {what} on request -- not retrying. {how}.",
        crate::dispatch_failure_ceiling::STOPPED_ON_REQUEST_MARKER
    );
    mcp.post_item_comment(item_id, body);
    let _ = writeln!(log, "{what}: {item_id}");
    WorkOutcome {
        exit_code: 1,
        retry_after_secs: None,
        fatal: true,
    }
}

/// Handles a run that failed because its agent is out (rate limited, out
/// of credit, usage window used up -- see `auth_runner::AgentFailure`).
/// `None` when `msg` isn't exhaustion-shaped, so the caller falls through
/// to the generic failure path.
///
/// - Short rate limit (known wait <= `SHORT_RATE_LIMIT_SECS`): cool the
///   agent down for that wait and retry the same agent after it.
/// - Otherwise, with `failover` on and `pick` finding an available agent:
///   release the claim, move the item to that agent (sticky), and end this
///   job terminally. The terminal-job hook puts the item back on
///   `ready-for-work` and the neutral `AGENT_FAILOVER_MARKER` comment keeps
///   the cycle out of the dispatch-failure ceiling.
/// - Otherwise: cool the agent down until its reset and schedule this job's
///   retry for then, with a neutral `AGENT_UNAVAILABLE_MARKER` comment.
#[allow(clippy::too_many_arguments)]
fn handle_agent_exhaustion(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    labels: &[String],
    agent: agent_registry::Agent,
    msg: &str,
    failover: bool,
    claim_guard: &mut ClaimGuard,
    notify_recipient: Option<&str>,
    log: &mut dyn std::io::Write,
    pick: impl FnOnce(
        &agentflare_backend::item::Item,
        &[String],
        agent_registry::Agent,
    ) -> Option<agent_registry::Agent>,
) -> Option<WorkOutcome> {
    use crate::auth_runner::AgentFailure;
    let failure = crate::auth_runner::classify_failure(msg);
    if !matches!(
        failure,
        AgentFailure::RateLimited { .. }
            | AgentFailure::CreditExhausted
            | AgentFailure::QuotaWindowExhausted { .. }
    ) {
        return None;
    }
    let now = chrono::Utc::now().timestamp();
    let wait = crate::quota::failover::mark_unavailable(agent.as_str(), &failure, now)
        .unwrap_or(crate::auth_runner::DEFAULT_RATE_LIMIT_SECS);
    if !failure.warrants_failover(now) {
        // A deliberate retry, not a failure: the neutral
        // `AGENT_UNAVAILABLE_MARKER` comment (not `release_and_comment`'s
        // "failed" one) keeps it out of the dispatch-failure ceiling.
        if release_claim(mcp, &item.id) {
            claim_guard.disarm();
        }
        let body = format!(
            "{}\n\n{} is {} -- retrying the same agent in {wait}s.\n\n{}",
            crate::dispatch_failure_ceiling::AGENT_UNAVAILABLE_MARKER,
            agent.as_str(),
            failure.describe(),
            tail_str(msg, DIAGNOSTIC_TAIL_CHARS)
        );
        mcp.post_item_comment(&item.id, &body);
        if let Some(recipient) = notify_recipient {
            notify(recipient, &body, &item.id);
        }
        let _ = writeln!(
            log,
            "{}: {} -- retrying the same agent in {wait}s",
            agent.as_str(),
            failure.describe()
        );
        return Some(WorkOutcome {
            exit_code: 1,
            retry_after_secs: Some(wait),
            fatal: false,
        });
    }

    let until = crate::quota::failover::format_unix(now + wait as i64);
    let reason = format!("{} -- unavailable until {until}", failure.describe());
    let released = release_claim(mcp, &item.id);
    if released {
        claim_guard.disarm();
    }
    if failover
        && released
        && let Some(to) = pick(item, labels, agent)
    {
        match crate::quota::failover::record_failover(mcp, &item.id, agent, to, &reason) {
            Ok(()) => {
                let _ = writeln!(
                    log,
                    "failover: {} is {reason}; moved item {} to {}",
                    agent.as_str(),
                    item.id,
                    to.as_str()
                );
                return Some(WorkOutcome {
                    exit_code: 1,
                    retry_after_secs: None,
                    fatal: true,
                });
            }
            Err(e) => {
                let _ = writeln!(
                    log,
                    "failover to {} failed ({e}); waiting instead",
                    to.as_str()
                );
            }
        }
    }

    let body = format!(
        "{}\n\n{} is {reason}. No other agent is available, so this run retries on {} \
         then.\n\n{}",
        crate::dispatch_failure_ceiling::AGENT_UNAVAILABLE_MARKER,
        agent.as_str(),
        agent.as_str(),
        tail_str(msg, DIAGNOSTIC_TAIL_CHARS)
    );
    mcp.post_item_comment(&item.id, &body);
    if let Some(recipient) = notify_recipient {
        notify(recipient, &body, &item.id);
    }
    let _ = writeln!(log, "{}: {reason}; retrying in {wait}s", agent.as_str());
    Some(WorkOutcome {
        exit_code: 1,
        retry_after_secs: Some(wait),
        fatal: false,
    })
}

/// Releases `item_id`'s claim; whether that succeeded.
fn release_claim(mcp: &AgentflareMcp, item_id: &str) -> bool {
    mcp.item_release(ItemRequest {
        action: "release".into(),
        id: Some(item_id.to_string()),
        ..Default::default()
    })
    .is_ok()
}

/// Before a daemon job claims anything: if the agent it would run is
/// already known to be unavailable (cooling down after an exhaustion, or
/// over its subscription's usage threshold), move the item to an available
/// agent and run that one instead of launching into a known wall. Returns
/// the agent name to run, or `None` to keep the original.
fn prelaunch_failover(
    item_id: &str,
    agent: &str,
    repo_root: Option<&std::path::Path>,
    log: &mut dyn std::io::Write,
) -> Option<String> {
    let from = agent_registry::agent_by_name(agent)?;
    let reason = match crate::quota::failover::unavailable_until(from.as_str()) {
        Some((until, why)) => format!(
            "{why} -- unavailable until {}",
            crate::quota::failover::format_unix(until)
        ),
        None if crate::quota::failover::over_usage_threshold(from) => {
            "over its usage threshold".to_string()
        }
        None => return None,
    };
    let mcp = match repo_root {
        Some(root) => AgentflareMcp::for_project_dir(root.to_path_buf()),
        None => AgentflareMcp::default(),
    };
    let item = mcp
        .with_backend_db(|conn| {
            let id = agentflare_backend::item::resolve_id(conn, None, item_id).ok()?;
            agentflare_backend::item::get(conn, &id).ok()
        })
        .ok()
        .flatten()?;
    let labels = crate::quota::failover::item_label_names(&mcp, &item.id);
    let to = crate::quota::failover::find_alternative(&item, &labels, from)?;
    crate::quota::failover::record_failover(&mcp, &item.id, from, to, &reason).ok()?;
    let _ = writeln!(
        log,
        "failover: {} is {reason}; running {} instead",
        from.as_str(),
        to.as_str()
    );
    Some(to.as_str().to_string())
}
