//! Background discovery loop: finds items labeled `ready-for-work` and
//! dispatches an in-process work job (`WorkItemExecutor`, running the same
//! logic as `agentflare work`) for each one whose assignee is a
//! confirmed-autonomous agent (skips the rest with a comment).

use crate::mcp_server::AgentflareMcp;
use crate::mcp_server::types::{CommentRequest, ItemRequest};

/// Also read by `mcp_server::handoff` — a freshly handed-off item is labeled
/// with this so the discovery loop below notices it without a human having
/// to add the label by hand. Single source of truth so the two can't drift.
pub(crate) const READY_LABEL: &str = "ready-for-work";
/// Also read by `dashboard::server::reconcile_orphaned_jobs` to swap a
/// crash-orphaned item back off `dispatched` -- single source of truth so
/// the two can't drift, same rationale as `READY_LABEL` above.
pub(crate) const DISPATCHED_LABEL: &str = "dispatched";
/// Also read by `dashboard::orphan_reconcile::handle_terminal_job_failure`
/// -- once `dispatch_failure_ceiling::DISPATCH_FAILURE_CAP` consecutive
/// dispatch cycles end with the same terminal failure reason, it lands here
/// rather than back on `READY_LABEL`, so it doesn't retry-loop against the
/// same broken agent (items #463/#506).
pub(crate) const NEEDS_MANUAL_LABEL: &str = "needs-manual-dispatch";
const NEEDS_HUMAN_GATE_LABEL: &str = "needs-human-gate";

/// Since item #19, work items run in-process via `WorkItemExecutor` rather
/// than as a spawned `agentflare work` subprocess, so this is no longer an
/// outer subprocess wall-clock kill -- it's the watchdog `run_in_process`
/// (agentflare-jobs' `worker.rs`) uses to abandon a stuck job (see its doc
/// comment) rather than let a hung coordination step wedge a worker thread
/// forever. `WorkArgs::DEFAULT_TIMEOUT_SECS` (21600s = 6h) is `agentflare
/// work`'s own hard-cap safety net -- not the primary judge of progress,
/// that's `--idle-timeout` (item #20) -- so this stays that budget plus
/// margin for the claim/worktree/done steps around it, exactly as when it
/// wrapped a real subprocess: it must never fire before the work being
/// watched would have stopped on its own.
pub(crate) const WORK_JOB_TIMEOUT_SECS: u64 = 21_900;

/// Returns the matching `Agent` only if `agent_registry::autonomous_args`
/// confirms it has a headless permission-bypass flag — the same gate
/// `agentflare work` itself uses (`src/cli/work.rs`'s `run_work`).
///
/// `assignee` may carry an instance suffix (`<agent>:<instance>`) once an
/// item has been claimed at least once — `item::claim` deliberately stores
/// the raw claim owner there (see its doc comment). Strip it via the same
/// `agent_part` the claim/handoff-freeze logic itself uses internally,
/// rather than matching the raw string and silently failing to recognize a
/// previously-claimed item's own assignee.
pub(crate) fn resolve_confirmed_agent(assignee: &str) -> Option<agent_registry::Agent> {
    let canonical = agentflare_backend::item::agent_part(assignee);
    let agent = agent_registry::REGISTRY
        .iter()
        .find(|s| s.id.as_str() == canonical)
        .map(|s| s.id)?;
    agent_registry::autonomous_args(agent).map(|_| agent)
}

pub(crate) struct DiscoveryTickResult {
    pub dispatched: usize,
    pub skipped: usize,
    /// `ready-for-work` items left labeled for a later tick (cooldown or a
    /// `Wait` decision) rather than dispatched or skipped-and-relabeled.
    /// Logged per-item below so an operator can see *why* an
    /// eligible-looking item didn't dispatch instead of the log staying
    /// silent tick after tick (item #82).
    pub waiting: usize,
}

/// Everything one project contributes to a discovery tick: its own
/// `ready-for-work` items plus the folder its worktrees must be created
/// under (from the `project_dirs` registry, not this process's cwd).
struct ProjectBatch {
    folder_path: String,
    items: Vec<agentflare_backend::item::Item>,
    label_id_by_name: std::collections::HashMap<String, String>,
    ready_id: String,
}

/// One pass: across every project registered in `project_dirs` (see
/// `AgentflareMcp::register_project_dir`, called wherever an agentflare
/// CLI/MCP call runs inside a linked repo) — not just whichever project
/// this daemon process happens to have been started from (item #63) —
/// list items labeled `ready-for-work`, dispatch a job for each one with a
/// confirmed-autonomous assignee, skip (+ comment + relabel) the rest. Ends
/// after enqueueing — it does not watch job completion, since `agentflare
/// work` itself reports outcome back onto the item.
pub(crate) fn run_discovery_tick(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    auth_conn: &rusqlite::Connection,
    host_policy: agentflare_resource_gate::Policy,
) -> DiscoveryTickResult {
    let mut result = DiscoveryTickResult {
        dispatched: 0,
        skipped: 0,
        waiting: 0,
    };

    let fetched = mcp.with_backend_db(|conn| {
        let dirs = agentflare_backend::project_dir::list(conn).ok()?;
        let mut batches = Vec::new();
        for dir in dirs {
            let labels = agentflare_backend::label::list_by_project(conn, &dir.project_id).ok()?;
            let mut label_id_by_name = std::collections::HashMap::new();
            for l in &labels {
                label_id_by_name.insert(l.name.clone(), l.id.clone());
            }
            // A project without the ready-for-work label (yet) has nothing
            // to discover — skip just this one, not the whole tick.
            let Some(ready_id) = label_id_by_name.get(READY_LABEL).cloned() else {
                continue;
            };
            let items =
                agentflare_backend::item::list_by_label(conn, &dir.project_id, &ready_id).ok()?;
            batches.push(ProjectBatch {
                folder_path: dir.folder_path,
                items,
                label_id_by_name,
                ready_id,
            });
        }
        Some(batches)
    });

    let Ok(Some(batches)) = fetched else {
        return result;
    };

    for batch in batches {
        let ProjectBatch {
            folder_path,
            items,
            label_id_by_name,
            ready_id,
        } = batch;
        for item in items {
            match crate::quota::decide::decide_for_supervisor(mcp, &item) {
                crate::quota::decide::EffectiveAction::Run
                | crate::quota::decide::EffectiveAction::SelfRepair => {
                    let Some(agent) = item
                        .assignee_agent
                        .as_deref()
                        .and_then(resolve_confirmed_agent)
                    else {
                        // decide() already checked eligibility (tier 5) before
                        // returning Run/SelfRepair, so this is unreachable in
                        // practice; treat it the same as the pre-existing skip
                        // path rather than panicking on a decision-vs-dispatch
                        // mismatch.
                        skip_item(mcp, &item, &label_id_by_name, &ready_id);
                        result.skipped += 1;
                        continue;
                    };
                    if crate::auth_db::is_cooling_down(auth_conn, agent.as_str()) {
                        // Leave the ready-for-work label in place, same as the
                        // Wait branch below: the cooldown may clear before the
                        // next tick, and the item must still be visible to that
                        // tick's discovery query.
                        eprintln!(
                            "agentflare-supervisor: item #{} ({}) is ready-for-work but agent '{}' is cooling down",
                            item.sequence_id,
                            item.id,
                            agent.as_str()
                        );
                        result.waiting += 1;
                        continue;
                    }
                    // Independent of the per-agent cooldown above: this is
                    // the host's own CPU-pressure tier, not agent identity.
                    // Both gates must pass before a dispatch proceeds.
                    if host_policy.blocks_dispatch() {
                        eprintln!(
                            "agentflare-supervisor: item #{} ({}) is ready-for-work but the host resource gate is {}",
                            item.sequence_id,
                            item.id,
                            host_policy.as_str()
                        );
                        result.waiting += 1;
                        continue;
                    }
                    if dispatch_item(
                        mcp,
                        queue,
                        &item,
                        agent,
                        &folder_path,
                        &label_id_by_name,
                        &ready_id,
                    ) {
                        result.dispatched += 1;
                    }
                }
                crate::quota::decide::EffectiveAction::Ask(question) => {
                    ask_item(mcp, &item, &question, &label_id_by_name, &ready_id);
                    result.skipped += 1;
                }
                crate::quota::decide::EffectiveAction::Wait(reason) => {
                    // Leave the ready-for-work label in place: the wait
                    // condition may clear before the next tick, and the item
                    // must still be visible to that tick's discovery query.
                    eprintln!(
                        "agentflare-supervisor: item #{} ({}) is ready-for-work but waiting: {reason}",
                        item.sequence_id, item.id
                    );
                    result.waiting += 1;
                }
                crate::quota::decide::EffectiveAction::StayQuiet => {
                    skip_item(mcp, &item, &label_id_by_name, &ready_id);
                    result.skipped += 1;
                }
            }
        }
    }
    result
}

fn skip_item(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    label_id_by_name: &std::collections::HashMap<String, String>,
    ready_id: &str,
) {
    let reason = match &item.assignee_agent {
        None => "no assignee_agent set — cannot auto-dispatch".to_string(),
        Some(a) => format!("assignee '{a}' is not a confirmed-autonomous agent"),
    };
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item.id.clone()),
        body: Some(format!(
            "## supervisor — skipped\n\n{reason}. Run `agentflare work` manually."
        )),
        ..Default::default()
    });
    let _ = mcp.item_remove_label(ItemRequest {
        action: "remove_label".into(),
        id: Some(item.id.clone()),
        label_id: Some(ready_id.to_string()),
        ..Default::default()
    });
    if let Some(needs_manual_id) = label_id_by_name.get(NEEDS_MANUAL_LABEL) {
        let _ = mcp.item_add_label(ItemRequest {
            action: "add_label".into(),
            id: Some(item.id.clone()),
            label_id: Some(needs_manual_id.clone()),
            ..Default::default()
        });
    }
}

fn ask_item(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    question: &str,
    label_id_by_name: &std::collections::HashMap<String, String>,
    ready_id: &str,
) {
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item.id.clone()),
        body: Some(format!("## supervisor — gated\n\n{question}")),
        ..Default::default()
    });
    let _ = mcp.item_remove_label(ItemRequest {
        action: "remove_label".into(),
        id: Some(item.id.clone()),
        label_id: Some(ready_id.to_string()),
        ..Default::default()
    });
    if let Some(gated_id) = label_id_by_name.get(NEEDS_HUMAN_GATE_LABEL) {
        let _ = mcp.item_add_label(ItemRequest {
            action: "add_label".into(),
            id: Some(item.id.clone()),
            label_id: Some(gated_id.clone()),
            ..Default::default()
        });
    }
}

/// Runs in-process via `WorkItemExecutor` (registered on the daemon's
/// `WorkerPool`, see `dashboard/server.rs::run`) instead of spawning a fresh
/// `agentflare work` subprocess — item #19. `command` is a display label
/// only (shown in the dashboard's job list); nothing spawns it, so master's
/// `current_exe()`-staleness fix (see git history) is moot here: there's no
/// exe path to resolve at all once dispatch never spawns one. `args` is
/// `[item_id, agent]`, plus `folder_path` when the caller has one (item
/// #63) — `WorkItemExecutor::execute` claims/worktrees against that folder
/// instead of wherever this daemon process happens to have started.
///
/// Shared by `dispatch_item` (a fresh `ready-for-work` item, always passes
/// its per-project `folder_path`) and `self_repair_or_gate` (item #65,
/// re-running the same job on an item already sitting in "in_review" --
/// `item_claim` reclaims its existing worktree/branch rather than starting
/// over, see `item::claim`'s doc comment). `run_review_sweep` itself now
/// also iterates every project in `project_dirs` (item #124, same pattern
/// `run_discovery_tick` already used for #63) and passes each project's own
/// `folder_path` down to `self_repair_or_gate`, so a self-repair job is
/// pinned to the correct project directory at dispatch time instead of
/// falling back to wherever the daemon process's ambient cwd happens to be
/// when the job actually runs.
/// Reads an optional `metadata.model` string override (settable via
/// `handoff`/`item update`'s `metadata` field) — the model the assigned
/// agent should use for this item's autonomous dispatch. No allowlist:
/// passed straight through to `--model <name>` (see `build_extra_args` in
/// `cli/work.rs`) — model catalogs change too often to hardcode, and the
/// underlying agent CLI already errors on an unknown name.
fn item_model_override(metadata: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(metadata)
        .ok()?
        .get("model")?
        .as_str()
        .map(str::to_string)
}

/// `dispatch_reason`, when set, is stashed on the job's `dispatch_reason`
/// metadata (see `AgentJob::dispatch_reason`) purely for dashboard display —
/// e.g. `self_repair_or_gate` passes the failing CI check name(s) so the
/// dashboard can badge *why* this run was fired instead of just that it was.
/// A fresh `dispatch_item` call passes `None`: it isn't reacting to anything,
/// it's just working the next ready item.
fn enqueue_work_job(
    queue: &agentflare_jobs::Queue,
    item: &agentflare_backend::item::Item,
    agent: agent_registry::Agent,
    folder_path: Option<&str>,
    dispatch_reason: Option<&str>,
) -> Option<agentflare_jobs::JobInfo> {
    let mut args = vec![item.id.clone(), agent.as_str().to_string()];
    if let Some(folder_path) = folder_path {
        args.push(folder_path.to_string());
        if let Some(model) = item_model_override(&item.metadata) {
            args.push(model);
        }
    }
    let mut job = agentflare_jobs::AgentJob::new("agentflare-work")
        .args(args)
        .timeout(WORK_JOB_TIMEOUT_SECS)
        .in_process();
    if let Some(reason) = dispatch_reason {
        job = job.dispatch_reason(reason);
    }
    queue.enqueue(&job).ok()
}

fn dispatch_item(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    item: &agentflare_backend::item::Item,
    agent: agent_registry::Agent,
    folder_path: &str,
    label_id_by_name: &std::collections::HashMap<String, String>,
    ready_id: &str,
) -> bool {
    let Some(info) = enqueue_work_job(queue, item, agent, Some(folder_path), None) else {
        return false;
    };

    let _ = mcp.item_remove_label(ItemRequest {
        action: "remove_label".into(),
        id: Some(item.id.clone()),
        label_id: Some(ready_id.to_string()),
        ..Default::default()
    });
    if let Some(dispatched_id) = label_id_by_name.get(DISPATCHED_LABEL) {
        let _ = mcp.item_add_label(ItemRequest {
            action: "add_label".into(),
            id: Some(item.id.clone()),
            label_id: Some(dispatched_id.clone()),
            ..Default::default()
        });
    }
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item.id.clone()),
        body: Some(format!(
            "{}\n\njob: {}",
            crate::dispatch_failure_ceiling::DISPATCH_MARKER,
            info.id
        )),
        ..Default::default()
    });
    true
}

/// Marker prefix on a self-repair-dispatch comment (see `self_repair_item`
/// below) -- `run_review_sweep` counts these on an item to enforce
/// `quota::decide::SELF_REPAIR_CAP` without a separate persistent counter,
/// the same way an item's `metadata` isn't otherwise touched by this file.
const CI_SELF_REPAIR_MARKER: &str = "## supervisor — CI self-repair dispatched";

pub(crate) struct ReviewSweepResult {
    pub promoted: usize,
    pub self_repaired: usize,
    pub skipped: usize,
    /// Items a later sweep should retry (agent cooling down, or the host
    /// resource gate throttling/pausing dispatch) rather than ones this
    /// sweep decided against. Mirrors `DiscoveryTickResult::waiting` — a
    /// deferral counted as "skipped" reads to an operator as a decision
    /// that won't be revisited, which is exactly backwards (item #82).
    pub waiting: usize,
}

/// Why `self_repair_or_gate` did or didn't dispatch. A plain `bool` can't
/// distinguish "decided against this item" from "try again next sweep".
enum SelfRepairOutcome {
    Dispatched,
    /// Retryable: the blocking condition (cooldown, host pressure) is
    /// expected to clear on its own.
    Deferred,
    Skipped,
}

/// Everything one project contributes to a review sweep: its own
/// `in_review` items, label lookup, and the folder its worktrees live
/// under (from the `project_dirs` registry, not this process's cwd) --
/// same shape `ProjectBatch` gives `run_discovery_tick`.
struct ReviewBatch {
    folder_path: String,
    items: Vec<agentflare_backend::item::Item>,
    label_id_by_name: std::collections::HashMap<String, String>,
}

/// One pass: across every project registered in `project_dirs` (mirrors
/// `run_discovery_tick`'s item #63 fix -- not just whichever project this
/// daemon process happens to have been started from) -- list items in the
/// "in_review" state group (an open PR), poll each one's PR/CI status, and
/// promote it to "completed" on a confirmed merge or dispatch a self-repair
/// job on failing CI (item #65). Unlike `run_discovery_tick`, there's no
/// label to gate the query on -- state group is itself the signal, and it's
/// also the concurrency guard: a self-repair job's `item_claim` moves the
/// item out of "in_review" into "started" for its duration (see
/// `item::claim`'s doc comment), so an item with a self-repair already
/// running never shows up here to be double-dispatched.
pub(crate) fn run_review_sweep(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    auth_conn: &rusqlite::Connection,
    host_policy: agentflare_resource_gate::Policy,
) -> ReviewSweepResult {
    let mut result = ReviewSweepResult {
        promoted: 0,
        self_repaired: 0,
        skipped: 0,
        waiting: 0,
    };

    let fetched = mcp.with_backend_db(|conn| {
        let dirs = agentflare_backend::project_dir::list(conn).ok()?;
        let mut batches = Vec::new();
        for dir in dirs {
            let items = agentflare_backend::item::list_by_project(conn, &dir.project_id).ok()?;
            let states = agentflare_backend::state::list_by_project(conn, &dir.project_id).ok()?;
            let state_by_id: std::collections::HashMap<&str, &agentflare_backend::state::State> =
                states.iter().map(|s| (s.id.as_str(), s)).collect();
            let in_review: Vec<_> = items
                .into_iter()
                .filter(|i| {
                    state_by_id
                        .get(i.state_id.as_str())
                        .is_some_and(|s| s.group_name == "in_review")
                })
                .collect();
            let labels = agentflare_backend::label::list_by_project(conn, &dir.project_id).ok()?;
            let mut label_id_by_name = std::collections::HashMap::new();
            for l in &labels {
                label_id_by_name.insert(l.name.clone(), l.id.clone());
            }
            batches.push(ReviewBatch {
                folder_path: dir.folder_path,
                items: in_review,
                label_id_by_name,
            });
        }
        Some(batches)
    });
    let Ok(Some(batches)) = fetched else {
        return result;
    };

    for batch in batches {
        let ReviewBatch {
            folder_path,
            items,
            label_id_by_name,
        } = batch;
        let repo_root = std::path::PathBuf::from(&folder_path);
        for item in &items {
            match crate::worktree::pr_ci_status(item, &repo_root) {
                crate::worktree::PrCiStatus::Merged => {
                    if promote_merged_item(mcp, item) {
                        result.promoted += 1;
                    } else {
                        result.skipped += 1;
                    }
                }
                crate::worktree::PrCiStatus::Failing(failed_checks) => {
                    match self_repair_or_gate(
                        mcp,
                        queue,
                        auth_conn,
                        host_policy,
                        item,
                        &failed_checks,
                        &label_id_by_name,
                        &folder_path,
                    ) {
                        SelfRepairOutcome::Dispatched => result.self_repaired += 1,
                        SelfRepairOutcome::Deferred => result.waiting += 1,
                        SelfRepairOutcome::Skipped => result.skipped += 1,
                    }
                }
                crate::worktree::PrCiStatus::Pending
                | crate::worktree::PrCiStatus::Passing
                | crate::worktree::PrCiStatus::Unknown => {
                    result.skipped += 1;
                }
            }
        }
    }
    result
}

fn promote_merged_item(mcp: &AgentflareMcp, item: &agentflare_backend::item::Item) -> bool {
    let Ok(json) = mcp.item_check_merge(ItemRequest {
        action: "check_merge".into(),
        id: Some(item.id.clone()),
        ..Default::default()
    }) else {
        return false;
    };
    serde_json::from_str::<serde_json::Value>(&json)
        .ok()
        .and_then(|v| v["promoted"].as_bool())
        .unwrap_or(false)
}

/// Whether an `agentflare-work` job is already queued or running for
/// `item_id` -- guards the (small) window between `enqueue_work_job`
/// returning and the job actually reaching `item_claim`, during which the
/// item's state group hasn't flipped out of "in_review" yet and a second
/// sweep tick could otherwise dispatch a duplicate.
fn job_in_flight(queue: &agentflare_jobs::Queue, item_id: &str) -> bool {
    [
        agentflare_jobs::JobState::Queued,
        agentflare_jobs::JobState::Running,
    ]
    .into_iter()
    .filter_map(|state| queue.list(Some(state)).ok())
    .flatten()
    .any(|job| job.args.contains(&item_id.to_string()))
}

/// Dispatches a self-repair job for an item whose PR has failing CI checks,
/// or -- once `quota::decide::SELF_REPAIR_CAP` prior attempts have been made
/// with no green build -- gates it for a human instead of retrying forever.
#[allow(clippy::too_many_arguments)]
fn self_repair_or_gate(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    auth_conn: &rusqlite::Connection,
    host_policy: agentflare_resource_gate::Policy,
    item: &agentflare_backend::item::Item,
    failed_checks: &[String],
    label_id_by_name: &std::collections::HashMap<String, String>,
    folder_path: &str,
) -> SelfRepairOutcome {
    let already_gated = label_id_by_name
        .get(NEEDS_HUMAN_GATE_LABEL)
        .is_some_and(|gate_id| {
            mcp.with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item.id))
                .ok()
                .and_then(Result::ok)
                .is_some_and(|ids| ids.contains(gate_id))
        });
    if already_gated || job_in_flight(queue, &item.id) {
        return SelfRepairOutcome::Skipped;
    }

    let prior_attempts = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item.id))
        .ok()
        .and_then(Result::ok)
        .map(|comments| {
            comments
                .iter()
                .filter(|c| c.body.starts_with(CI_SELF_REPAIR_MARKER))
                .count() as u32
        })
        .unwrap_or(0);

    if prior_attempts >= crate::quota::decide::SELF_REPAIR_CAP {
        let _ = mcp.comment_impl(CommentRequest {
            action: "create".into(),
            item_id: Some(item.id.clone()),
            body: Some(format!(
                "## supervisor — CI self-repair cap reached\n\nFailing checks: {}. \
                 {} automatic repair attempt(s) already made with no green build — needs a human look.",
                failed_checks.join(", "),
                crate::quota::decide::SELF_REPAIR_CAP,
            )),
            ..Default::default()
        });
        if let Some(gate_id) = label_id_by_name.get(NEEDS_HUMAN_GATE_LABEL) {
            let _ = mcp.item_add_label(ItemRequest {
                action: "add_label".into(),
                id: Some(item.id.clone()),
                label_id: Some(gate_id.clone()),
                ..Default::default()
            });
        }
        return SelfRepairOutcome::Skipped;
    }

    // Item #114: while the item's claim is still live (within its
    // #108-capped in_review TTL), nobody can actually reclaim it yet --
    // dispatching now would just die instantly at `execute_work`'s own
    // claim-acquire step, the same check performed here, downstream of
    // this function. Defer instead so the sweep retries once the claim
    // goes stale, rather than burning a cap slot (and posting a
    // self-repair-dispatched comment) on an attempt that never had a
    // chance to run.
    let claim_still_live = mcp
        .with_backend_db(|conn| {
            let requested_ttl = crate::mcp_server::types::backend_claim_ttl_secs();
            let ttl = agentflare_backend::claim::effective_ttl_secs(conn, &item.id, requested_ttl);
            agentflare_backend::claim::has_active_claim_by_other(
                conn,
                &item.id,
                "",
                crate::claims::now(),
                ttl,
            )
        })
        .ok()
        .and_then(Result::ok)
        .unwrap_or(false);
    if claim_still_live {
        return SelfRepairOutcome::Deferred;
    }

    let Some(agent) = item
        .assignee_agent
        .as_deref()
        .and_then(resolve_confirmed_agent)
    else {
        return SelfRepairOutcome::Skipped;
    };
    if crate::auth_db::is_cooling_down(auth_conn, agent.as_str()) {
        return SelfRepairOutcome::Deferred;
    }
    // Independent of the per-agent cooldown above — the host's own
    // CPU-pressure tier. Both gates must pass.
    if host_policy.blocks_dispatch() {
        return SelfRepairOutcome::Deferred;
    }
    let reason = format!("self-repair: {}", failed_checks.join(", "));
    let Some(info) = enqueue_work_job(queue, item, agent, Some(folder_path), Some(&reason)) else {
        return SelfRepairOutcome::Skipped;
    };
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item.id.clone()),
        body: Some(format!(
            "{CI_SELF_REPAIR_MARKER}\n\nCI is failing on this PR: {}.\n\n\
             Please investigate and push a fix.\n\njob: {}",
            failed_checks.join(", "),
            info.id,
        )),
        ..Default::default()
    });
    SelfRepairOutcome::Dispatched
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
