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
/// and `restore_ready_for_work` -- once `dispatch_failure_ceiling`'s
/// identical-reason or any-reason cap trips, it lands here rather than back
/// on `READY_LABEL`, so it doesn't retry-loop against the same broken agent
/// or a persistently orphaning job (items #463/#506/#164).
pub(crate) const NEEDS_MANUAL_LABEL: &str = "needs-manual-dispatch";
const NEEDS_HUMAN_GATE_LABEL: &str = "needs-human-gate";
/// Blocks auto-dispatch even while `READY_LABEL` is also present -- for a
/// go/no-go candidate item whose description says "not dispatched, awaiting
/// decision" but which was created (or handed off) with `ready-for-work`
/// attached anyway. That prose was previously the *only* gate, which nothing
/// actually enforced: `run_discovery_tick` dispatches on `READY_LABEL` alone,
/// so items #184/#185/#186/#187 (all four go/no-go candidates from #166's
/// spec) got auto-dispatched and re-dispatched across multiple agents dozens
/// of times before anyone made the call. Removing `READY_LABEL` isn't
/// enough on its own either -- `redispatch` re-attaches it unconditionally
/// (see `item::claim::REDISPATCH_CLEARED_LABELS`) -- so this label is a
/// belt-and-suspenders check that survives that path too, cleared only once
/// a human actually decides (remove the label, or `redispatch`
/// after removing it).
const NEEDS_DECISION_LABEL: &str = "needs-decision";

/// GitHub label a human applies to a CI-green PR to explicitly sign off on
/// `run_review_sweep`'s `Passing` branch auto-merging it (item #194). CI
/// green is what routes an item into that branch in the first place, so
/// this label only ever adds a gate on top of CI, never bypasses it --
/// mirrors item #192's "never bypass CI" principle for the duplicate-PR
/// guard. Single named constant so the label convention has one place to
/// rename.
const PR_APPROVAL_LABEL: &str = "status:pr:approved";

/// `vault` secret holding the Telegram chat id human-gate pings go to.
/// Reuses the same `channels`/`vault` path as `agentflare channel send`
/// rather than inventing a separate config store for one setting -- set it
/// with `agentflare vault set telegram_notify_chat_id <chat_id>` alongside
/// `telegram_bot_token` (see `channels::Platform::secret_name`).
const TELEGRAM_NOTIFY_CHAT_ID_SECRET: &str = "telegram_notify_chat_id";

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
            if let Some(gate_id) = label_id_by_name.get(NEEDS_DECISION_LABEL) {
                let gated = mcp
                    .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item.id))
                    .ok()
                    .and_then(Result::ok)
                    .is_some_and(|ids| ids.contains(gate_id));
                if gated {
                    eprintln!(
                        "agentflare-supervisor: item #{} ({}) is ready-for-work but gated pending a go/no-go decision ({NEEDS_DECISION_LABEL})",
                        item.sequence_id, item.id
                    );
                    if first_time_gated(&item.id) {
                        notify_human_gate(&item, "gated pending a go/no-go decision");
                    }
                    result.waiting += 1;
                    continue;
                }
            }
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
    notify_human_gate(item, question);
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
                crate::worktree::PrCiStatus::Passing { number, labels } => {
                    if merge_if_approved(mcp, item, &repo_root, number, &labels) {
                        result.promoted += 1;
                    } else {
                        result.skipped += 1;
                    }
                }
                crate::worktree::PrCiStatus::Pending | crate::worktree::PrCiStatus::Unknown => {
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

/// Auto-merges a CI-green PR and promotes its item, but only once a human
/// has attached `PR_APPROVAL_LABEL` to the PR itself -- checked first and
/// short-circuits before any GitHub call so an unapproved item never touches
/// the network here. Only ever called from `run_review_sweep`'s `Passing`
/// arm, so CI green is structurally required: the label can add a gate on
/// top of it, never bypass it.
fn merge_if_approved(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    repo_root: &std::path::Path,
    number: u64,
    labels: &[String],
) -> bool {
    if !labels.iter().any(|l| l == PR_APPROVAL_LABEL) {
        return false;
    }
    let Some(repo) = crate::github::RepoId::resolve_from_remote(repo_root) else {
        return false;
    };
    let Ok(client) = crate::github::Client::new() else {
        return false;
    };
    merge_approved_pr(&client, &repo, number) && promote_merged_item(mcp, item)
}

/// The actual GitHub merge call for an approved, CI-green PR. Split out from
/// `merge_if_approved` so tests can drive it against a mock server instead
/// of `Client::new()`'s real credentials/host, mirroring `github::pulls`'
/// own test style. Squash matches this repo's existing single-commit-per-item
/// convention. Logs and falls through (never retries in-line) on failure --
/// branch protection or a merge conflict just means the item sits until the
/// next sweep tick, same as any other `skipped` outcome.
fn merge_approved_pr(
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    number: u64,
) -> bool {
    match crate::github::pulls::merge(client, repo, number, "squash") {
        Ok(()) => true,
        Err(e) => {
            eprintln!("agentflare-supervisor: auto-merge failed for PR #{number} in {repo}: {e}");
            false
        }
    }
}

/// Called from `item_check_merge` right after `item_id` is promoted to
/// `completed` (both the automatic path via `promote_merged_item` above and
/// manual/reconciliation calls funnel through that one function) -- for
/// every item that declared a dependency on `item_id`, once *all* of its
/// dependencies are completed, apply `READY_LABEL` so `run_discovery_tick`
/// picks it up without a human/PM having to notice and `handoff` it by hand
/// (item #195).
///
/// Idempotent and safe under concurrent sibling completions:
/// `item::add_label`'s `INSERT OR IGNORE` makes re-labeling a no-op, and an
/// already-`dispatched` item won't be relabeled `ready-for-work` by this
/// (it only ever adds the ready label, never touches `dispatched`).
pub(crate) fn cascade_unblock_dependents(conn: &rusqlite::Connection, item_id: &str) {
    let Ok(dependents) = agentflare_backend::item::dependents_of(conn, item_id) else {
        return;
    };
    for dependent_id in dependents {
        if !agentflare_backend::item::all_dependencies_completed(conn, &dependent_id)
            .unwrap_or(false)
        {
            continue;
        }
        let Ok(dependent) = agentflare_backend::item::get(conn, &dependent_id) else {
            continue;
        };
        if dependent.assignee_agent.is_none() {
            // A ready-for-work item with no assignee just sits inert in
            // run_discovery_tick (it only dispatches a resolvable
            // assignee_agent) -- labeling it here would be a silent no-op,
            // so skip it loudly instead so a PM/human notices and assigns it.
            eprintln!(
                "agentflare-supervisor: item #{} ({}) has all dependencies completed but no assignee_agent -- not auto-labeled {READY_LABEL}, needs manual dispatch",
                dependent.sequence_id, dependent.id
            );
            continue;
        }
        let Ok(labels) = agentflare_backend::label::list_by_project(conn, &dependent.project_id)
        else {
            continue;
        };
        let Some(ready_id) = labels.into_iter().find(|l| l.name == READY_LABEL).map(|l| l.id)
        else {
            continue;
        };
        match agentflare_backend::item::add_label(conn, &dependent.id, &ready_id) {
            Ok(()) => eprintln!(
                "agentflare-supervisor: item #{} ({}) all dependencies completed -- auto-labeled {READY_LABEL}",
                dependent.sequence_id, dependent.id
            ),
            Err(e) => eprintln!(
                "agentflare-supervisor: failed to auto-label item #{} ({}) {READY_LABEL} after dependency {item_id} completed: {e}",
                dependent.sequence_id, dependent.id
            ),
        }
    }
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

/// Best-effort Telegram ping for an item that just landed on a human gate
/// (a go/no-go decision, an unanswerable question, or a CI self-repair cap).
/// Silently does nothing when `TELEGRAM_NOTIFY_CHAT_ID_SECRET` isn't
/// configured, since notifications are opt-in and a bare install shouldn't
/// spam stderr every tick; a configured-but-failing send only logs -- a
/// notification failure must never block the gate itself.
fn notify_human_gate(item: &agentflare_backend::item::Item, reason: &str) {
    let Ok(Some(chat_id)) = crate::vault::get_secret(TELEGRAM_NOTIFY_CHAT_ID_SECRET) else {
        return;
    };
    let text = format!(
        "agentflare: item #{} ({}) needs a human -- {reason}",
        item.sequence_id, item.id
    );
    if let Err(e) =
        crate::channels::send_message(crate::channels::Platform::Telegram, &chat_id, &text)
    {
        eprintln!(
            "agentflare-supervisor: telegram notify failed for item #{}: {e}",
            item.sequence_id
        );
    }
}

/// True the first time a given item id is seen gated since this process
/// started, false on every later call for the same id -- `run_discovery_tick`
/// re-visits an already-gated item on every tick (it stays in the
/// `ready-for-work` query until a human clears `NEEDS_DECISION_LABEL`), so
/// this keeps `notify_human_gate` firing once per gate instead of once per
/// tick. In-memory and per-process by design: a daemon restart re-notifies
/// once, which is preferable to a persistent marker for a one-line ping.
fn first_time_gated(item_id: &str) -> bool {
    static NOTIFIED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
        std::sync::OnceLock::new();
    NOTIFIED
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(item_id.to_string())
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
        notify_human_gate(
            item,
            &format!(
                "CI self-repair cap reached ({} attempt(s), still failing: {})",
                crate::quota::decide::SELF_REPAIR_CAP,
                failed_checks.join(", ")
            ),
        );
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
