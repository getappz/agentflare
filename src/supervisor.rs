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
const DISPATCHED_LABEL: &str = "dispatched";
const NEEDS_MANUAL_LABEL: &str = "needs-manual-dispatch";
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
const WORK_JOB_TIMEOUT_SECS: u64 = 21_900;

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
/// its per-project `folder_path`) and `run_review_sweep`'s self-repair path
/// (item #65, re-running the same job on an item already sitting in
/// "in_review" -- `item_claim` reclaims its existing worktree/branch rather
/// than starting over, see `item::claim`'s doc comment). `run_review_sweep`
/// itself is still single-project (`resolve_project`'s cwd-based
/// resolution), so it has no folder path of its own to pass yet.
fn enqueue_work_job(
    queue: &agentflare_jobs::Queue,
    item: &agentflare_backend::item::Item,
    agent: agent_registry::Agent,
    folder_path: Option<&str>,
) -> Option<agentflare_jobs::JobInfo> {
    let mut args = vec![item.id.clone(), agent.as_str().to_string()];
    if let Some(folder_path) = folder_path {
        args.push(folder_path.to_string());
    }
    let job = agentflare_jobs::AgentJob::new("agentflare-work")
        .args(args)
        .timeout(WORK_JOB_TIMEOUT_SECS)
        .in_process();
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
    let Some(info) = enqueue_work_job(queue, item, agent, Some(folder_path)) else {
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
        body: Some(format!("## supervisor — dispatched\n\njob: {}", info.id)),
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
}

/// One pass: list items in the "in_review" state group (an open PR), poll
/// each one's PR/CI status, and promote it to "completed" on a confirmed
/// merge or dispatch a self-repair job on failing CI (item #65). Unlike
/// `run_discovery_tick`, there's no label to gate the query on -- state
/// group is itself the signal, and it's also the concurrency guard: a
/// self-repair job's `item_claim` moves the item out of "in_review" into
/// "started" for its duration (see `item::claim`'s doc comment), so an item
/// with a self-repair already running never shows up here to be
/// double-dispatched.
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
    };

    let fetched = mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).ok()?;
        let items = agentflare_backend::item::list_by_project(conn, &project.id).ok()?;
        let states = agentflare_backend::state::list_by_project(conn, &project.id).ok()?;
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
        let labels = agentflare_backend::label::list_by_project(conn, &project.id).ok()?;
        let mut label_id_by_name = std::collections::HashMap::new();
        for l in &labels {
            label_id_by_name.insert(l.name.clone(), l.id.clone());
        }
        Some((in_review, label_id_by_name))
    });
    let Ok(Some((items, label_id_by_name))) = fetched else {
        return result;
    };

    let repo_root = mcp.worktree_repo_root();
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
                if self_repair_or_gate(
                    mcp,
                    queue,
                    auth_conn,
                    host_policy,
                    item,
                    &failed_checks,
                    &label_id_by_name,
                ) {
                    result.self_repaired += 1;
                } else {
                    result.skipped += 1;
                }
            }
            crate::worktree::PrCiStatus::Pending
            | crate::worktree::PrCiStatus::Passing
            | crate::worktree::PrCiStatus::Unknown => {
                result.skipped += 1;
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
/// Returns whether a repair job was dispatched.
fn self_repair_or_gate(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    auth_conn: &rusqlite::Connection,
    host_policy: agentflare_resource_gate::Policy,
    item: &agentflare_backend::item::Item,
    failed_checks: &[String],
    label_id_by_name: &std::collections::HashMap<String, String>,
) -> bool {
    let already_gated = label_id_by_name
        .get(NEEDS_HUMAN_GATE_LABEL)
        .is_some_and(|gate_id| {
            mcp.with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item.id))
                .ok()
                .and_then(Result::ok)
                .is_some_and(|ids| ids.contains(gate_id))
        });
    if already_gated || job_in_flight(queue, &item.id) {
        return false;
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
        return false;
    }

    let Some(agent) = item
        .assignee_agent
        .as_deref()
        .and_then(resolve_confirmed_agent)
    else {
        return false;
    };
    if crate::auth_db::is_cooling_down(auth_conn, agent.as_str()) {
        return false;
    }
    // Independent of the per-agent cooldown above — the host's own
    // CPU-pressure tier. Both gates must pass.
    if host_policy.blocks_dispatch() {
        return false;
    }
    let Some(info) = enqueue_work_job(queue, item, agent, None) else {
        return false;
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
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_confirmed_agent_accepts_claude_code() {
        assert_eq!(
            resolve_confirmed_agent("claude-code"),
            Some(agent_registry::Agent::ClaudeCode)
        );
    }

    #[test]
    fn resolve_confirmed_agent_recognizes_an_instance_suffixed_assignee() {
        // A previously-claimed item's assignee_agent carries `<agent>:<instance>`
        // (see item::claim's doc comment) — this must still resolve, or a
        // once-claimed item can never be redispatched.
        assert_eq!(
            resolve_confirmed_agent("claude-code:some-job-id"),
            Some(agent_registry::Agent::ClaudeCode)
        );
    }

    #[test]
    fn resolve_confirmed_agent_rejects_opencode() {
        assert_eq!(resolve_confirmed_agent("opencode"), None);
    }

    #[test]
    fn resolve_confirmed_agent_rejects_unknown_agent_string() {
        assert_eq!(resolve_confirmed_agent("not-a-real-agent"), None);
    }

    fn test_mcp() -> AgentflareMcp {
        AgentflareMcp::for_test_memory()
    }

    fn test_queue() -> agentflare_jobs::Queue {
        let dir = tempfile::tempdir().unwrap().keep();
        agentflare_jobs::Queue::open_memory(dir.join("logs")).unwrap()
    }

    fn test_auth_conn() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::auth_db::migrate(&conn).unwrap();
        conn
    }

    fn seed_ready_item(mcp: &AgentflareMcp, assignee: Option<&str>) -> String {
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            for name in ["ready-for-work", "dispatched", "needs-manual-dispatch"] {
                agentflare_backend::label::create(
                    conn,
                    agentflare_backend::label::CreateLabel {
                        project_id: Some(project.id.clone()),
                        workspace_id: project.workspace_id.clone(),
                        name: name.into(),
                        color: None,
                        parent_id: None,
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                    },
                )
                .unwrap();
            }
            let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
            let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id,
                    name: "Do the thing".into(),
                    description: Some("do it well".into()),
                    priority: None,
                    parent_id: None,
                    assignee_agent: assignee.map(str::to_string),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: None,
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();
            let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
            let ready_id = &labels
                .iter()
                .find(|l| l.name == "ready-for-work")
                .unwrap()
                .id;
            agentflare_backend::item::add_label(conn, &item.id, ready_id).unwrap();
            item.id
        })
        .unwrap()
    }

    fn labels_contain_name(mcp: &AgentflareMcp, label_ids: &[String], name: &str) -> bool {
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            let all = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
            let target = all.iter().find(|l| l.name == name).unwrap();
            label_ids.contains(&target.id)
        })
        .unwrap()
    }

    #[test]
    fn confirmed_agent_gets_dispatched_and_relabeled() {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_ready_item(&mcp, Some("claude-code"));

        let auth_conn = test_auth_conn();
        let result = run_discovery_tick(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(result.dispatched, 1);
        assert_eq!(result.skipped, 0);

        let jobs = queue.list(None).unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(
            jobs[0].in_process,
            "work-item jobs must run in-process (item #19)"
        );
        assert!(jobs[0].args.contains(&item_id));
        assert!(jobs[0].args.contains(&"claude-code".to_string()));

        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(!labels_contain_name(&mcp, &labels, "ready-for-work"));
        assert!(labels_contain_name(&mcp, &labels, "dispatched"));
    }

    #[test]
    fn agent_in_cooldown_is_skipped_not_dispatched() {
        let mcp = test_mcp();
        let queue = test_queue();
        let auth_conn = test_auth_conn();
        let item_id = seed_ready_item(&mcp, Some("claude-code"));
        crate::auth_db::set_cooldown(&auth_conn, "claude-code", "__default__", 30, "rate limit");

        let result = run_discovery_tick(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(result.dispatched, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(
            result.waiting, 1,
            "a cooling-down item must count as waiting, not vanish silently (item #82)"
        );
        assert!(
            queue.list(None).unwrap().is_empty(),
            "a cooling-down agent must not be dispatched"
        );

        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(
            labels_contain_name(&mcp, &labels, "ready-for-work"),
            "the item must stay ready-for-work so a later tick can pick it up once the cooldown clears"
        );
    }

    #[path = "../../supervisor_host_gate_tests.rs"]
    mod host_gate_tests;

    fn seed_ready_item_under_gated_goal(mcp: &AgentflareMcp) -> String {
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            for name in [
                "ready-for-work",
                "dispatched",
                "needs-manual-dispatch",
                NEEDS_HUMAN_GATE_LABEL,
            ] {
                let _ = agentflare_backend::label::create(
                    conn,
                    agentflare_backend::label::CreateLabel {
                        project_id: Some(project.id.clone()),
                        workspace_id: project.workspace_id.clone(),
                        name: name.into(),
                        color: None,
                        parent_id: None,
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                    },
                );
            }
            let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
            let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
            let goal_metadata = serde_json::json!({
                "goal": {
                    "objective": "ship it",
                    "scope": { "allowed_paths": [], "disallowed_actions": [] },
                    "quota_mode": "default",
                    "lifecycle": "gated",
                    "consecutive_self_repairs": 0,
                }
            })
            .to_string();
            let goal_item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id: state_id.clone(),
                    name: "goal".into(),
                    description: None,
                    priority: None,
                    parent_id: None,
                    assignee_agent: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: Some(goal_metadata),
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id,
                    name: "Do the thing".into(),
                    description: Some("do it well".into()),
                    priority: None,
                    parent_id: Some(goal_item.id.clone()),
                    assignee_agent: Some("claude-code".into()),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: None,
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();
            let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
            let ready_id = &labels
                .iter()
                .find(|l| l.name == "ready-for-work")
                .unwrap()
                .id;
            agentflare_backend::item::add_label(conn, &item.id, ready_id).unwrap();
            item.id
        })
        .unwrap()
    }

    #[test]
    fn gated_goal_never_dispatches_and_relabels_to_needs_human_gate() {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_ready_item_under_gated_goal(&mcp);

        let auth_conn = test_auth_conn();
        let result = run_discovery_tick(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(result.dispatched, 0);
        assert!(
            queue.list(None).unwrap().is_empty(),
            "an ask decision must never enqueue a job"
        );

        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(!labels_contain_name(&mcp, &labels, "ready-for-work"));
        assert!(labels_contain_name(&mcp, &labels, NEEDS_HUMAN_GATE_LABEL));
    }

    fn seed_ready_item_under_active_goal_with_repairs(
        mcp: &AgentflareMcp,
        repairs: u32,
    ) -> (String, String) {
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            for name in [
                "ready-for-work",
                "dispatched",
                "needs-manual-dispatch",
                NEEDS_HUMAN_GATE_LABEL,
            ] {
                let _ = agentflare_backend::label::create(
                    conn,
                    agentflare_backend::label::CreateLabel {
                        project_id: Some(project.id.clone()),
                        workspace_id: project.workspace_id.clone(),
                        name: name.into(),
                        color: None,
                        parent_id: None,
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                    },
                );
            }
            let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
            let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
            let goal_metadata = serde_json::json!({
                "goal": {
                    "objective": "ship it",
                    "scope": { "allowed_paths": [], "disallowed_actions": [] },
                    "quota_mode": "default",
                    "lifecycle": "active",
                    "consecutive_self_repairs": repairs,
                }
            })
            .to_string();
            let goal_item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id: state_id.clone(),
                    name: "goal".into(),
                    description: None,
                    priority: None,
                    parent_id: None,
                    assignee_agent: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: Some(goal_metadata),
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();
            agentflare_backend::vent::upsert(
                conn,
                &project.id,
                "minor friction",
                "low",
                "[]",
                "topic",
                "evt-1",
                1,
                crate::claims::now(),
            )
            .unwrap();
            let vents = agentflare_backend::vent::list(conn, &project.id, false).unwrap();
            agentflare_backend::vent::set_actionable(conn, &vents[0].id, true).unwrap();
            agentflare_backend::vent::link_item(conn, &vents[0].id, &goal_item.id).unwrap();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id,
                    name: "Do the thing".into(),
                    description: Some("do it well".into()),
                    priority: None,
                    parent_id: Some(goal_item.id.clone()),
                    assignee_agent: Some("claude-code".into()),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: None,
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();
            let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
            let ready_id = &labels
                .iter()
                .find(|l| l.name == "ready-for-work")
                .unwrap()
                .id;
            agentflare_backend::item::add_label(conn, &item.id, ready_id).unwrap();
            (item.id, goal_item.id)
        })
        .unwrap()
    }

    #[test]
    fn under_cap_self_repairs_and_dispatches() {
        let mcp = test_mcp();
        let queue = test_queue();
        let (_item_id, _goal_id) = seed_ready_item_under_active_goal_with_repairs(&mcp, 0);

        let auth_conn = test_auth_conn();
        let result = run_discovery_tick(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(result.dispatched, 1, "self-repair still dispatches the job");
        assert_eq!(queue.list(None).unwrap().len(), 1);
    }

    #[test]
    fn at_cap_forces_ask_instead_of_dispatching() {
        let mcp = test_mcp();
        let queue = test_queue();
        let (item_id, _goal_id) = seed_ready_item_under_active_goal_with_repairs(
            &mcp,
            crate::quota::decide::SELF_REPAIR_CAP,
        );

        let auth_conn = test_auth_conn();
        let result = run_discovery_tick(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(
            result.dispatched, 0,
            "the cap must force ask, not another self-repair dispatch"
        );
        assert!(queue.list(None).unwrap().is_empty());
        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(labels_contain_name(&mcp, &labels, NEEDS_HUMAN_GATE_LABEL));
    }

    #[test]
    fn ungrouped_ready_item_dispatches_exactly_as_before_this_change() {
        let mcp = test_mcp();
        let queue = test_queue();
        // Reuses the pre-existing seed_ready_item helper (no goal ancestor
        // at all) — this is the plan's explicit no-regression guarantee.
        let item_id = seed_ready_item(&mcp, Some("claude-code"));

        let auth_conn = test_auth_conn();
        let result = run_discovery_tick(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(result.dispatched, 1);
        assert_eq!(result.skipped, 0);
        let jobs = queue.list(None).unwrap();
        assert!(jobs[0].args.contains(&item_id));
    }

    #[test]
    fn unconfirmed_agent_gets_skipped_not_dispatched() {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_ready_item(&mcp, Some("opencode"));

        let auth_conn = test_auth_conn();
        let result = run_discovery_tick(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(result.dispatched, 0);
        assert_eq!(result.skipped, 1);
        assert!(queue.list(None).unwrap().is_empty());

        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(!labels_contain_name(&mcp, &labels, "ready-for-work"));
        assert!(labels_contain_name(&mcp, &labels, "needs-manual-dispatch"));
    }

    /// Seeds a ready-for-work item in a brand new project/workspace
    /// (independent of whatever `test_mcp()`'s cwd-resolved project is) and
    /// registers it in `project_dirs` at `folder_path` — the same registry
    /// `AgentflareMcp::register_project_dir` populates for a real repo, but
    /// written directly here so the test controls the folder path without
    /// needing a real linked repo on disk.
    fn seed_ready_item_in_project(mcp: &AgentflareMcp, name: &str, folder_path: &str) -> String {
        mcp.with_backend_db(|conn| {
            let workspace = agentflare_backend::workspace::create(
                conn,
                agentflare_backend::workspace::CreateWorkspace {
                    name: name.into(),
                    slug: name.into(),
                    owner_agent: None,
                    item_label: None,
                },
            )
            .unwrap();
            let project = agentflare_backend::project::create(
                conn,
                agentflare_backend::project::CreateProject {
                    workspace_id: workspace.id,
                    name: name.into(),
                    identifier: name.into(),
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap();
            agentflare_backend::project_dir::upsert(conn, &project.id, folder_path, 1).unwrap();
            for label_name in ["ready-for-work", "dispatched", "needs-manual-dispatch"] {
                agentflare_backend::label::create(
                    conn,
                    agentflare_backend::label::CreateLabel {
                        project_id: Some(project.id.clone()),
                        workspace_id: project.workspace_id.clone(),
                        name: label_name.into(),
                        color: None,
                        parent_id: None,
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                    },
                )
                .unwrap();
            }
            let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
            let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id,
                    name: "Do the thing".into(),
                    description: Some("do it well".into()),
                    priority: None,
                    parent_id: None,
                    assignee_agent: Some("claude-code".into()),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: None,
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();
            let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
            let ready_id = &labels
                .iter()
                .find(|l| l.name == "ready-for-work")
                .unwrap()
                .id;
            agentflare_backend::item::add_label(conn, &item.id, ready_id).unwrap();
            item.id
        })
        .unwrap()
    }

    // --- run_review_sweep / self_repair_or_gate (item #65) ---

    /// A throwaway git repo with no remote -- same trick
    /// `item_check_merge_leaves_an_in_review_item_alone_when_merge_status_is_unknown`
    /// (in `mcp_server::tests::action_tests`) uses: `RepoId::resolve_from_remote`
    /// soft-fails to `None` against it, so `worktree::pr_ci_status` reports
    /// `Unknown` without ever touching the network.
    fn throwaway_repo() -> tempfile::TempDir {
        let repo_dir = tempfile::tempdir().unwrap();
        let run_git = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(repo_dir.path())
                .output()
                .unwrap()
        };
        run_git(&["init", "-b", "master"]);
        run_git(&["config", "user.email", "test@test.com"]);
        run_git(&["config", "user.name", "Test"]);
        run_git(&["commit", "--allow-empty", "-m", "initial"]);
        repo_dir
    }

    fn test_mcp_with_repo(repo_root: std::path::PathBuf) -> AgentflareMcp {
        AgentflareMcp::for_test(
            repo_root.join("backend.db"),
            repo_root.clone(),
            repo_root.join("project.json"),
        )
    }

    /// Seeds an item already sitting in "in_review" -- claimed and moved
    /// there directly via the backend calls `item_claim`/`item_done` wrap
    /// (bypassing worktree creation entirely, unlike `seed_ready_item`'s
    /// real-claim-through-the-MCP-method path), since these tests exercise
    /// `run_review_sweep`'s decision logic, not the claim/worktree mechanics
    /// already covered by `mcp_server::tests`.
    fn seed_in_review_item(mcp: &AgentflareMcp, assignee: Option<&str>) -> String {
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
            let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id,
                    name: "Fix CI".into(),
                    description: Some("do it well".into()),
                    priority: None,
                    parent_id: None,
                    assignee_agent: assignee.map(str::to_string),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: None,
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();
            let owner = assignee
                .map(|a| format!("{a}:prior-job"))
                .unwrap_or_else(|| "cli".into());
            agentflare_backend::item::claim(
                conn,
                &item.id,
                &owner,
                crate::claims::now(),
                crate::claims::ttl_secs(),
            )
            .unwrap();
            agentflare_backend::item::mark_in_review(conn, &item.id, &owner).unwrap();
            item.id
        })
        .unwrap()
    }

    #[test]
    fn run_discovery_tick_dispatches_ready_items_from_every_registered_project_not_just_one() {
        // Item #63: the daemon's own cwd-resolved project must not be the
        // only project discovery ever looks at — every project registered
        // in `project_dirs` (populated by any CLI/MCP call that ever ran
        // inside it) must get its ready-for-work items picked up too.
        let mcp = test_mcp();
        let queue = test_queue();
        let item_a = seed_ready_item_in_project(&mcp, "proj-a", "/repo/a");
        let item_b = seed_ready_item_in_project(&mcp, "proj-b", "/repo/b");

        let auth_conn = test_auth_conn();
        let result = run_discovery_tick(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(
            result.dispatched, 2,
            "both projects' ready items must be dispatched, not just one"
        );
        let jobs = queue.list(None).unwrap();
        assert_eq!(jobs.len(), 2);

        let job_a = jobs.iter().find(|j| j.args.contains(&item_a)).unwrap();
        assert!(
            job_a.args.contains(&"/repo/a".to_string()),
            "job for proj-a's item must carry proj-a's own folder path, got {:?}",
            job_a.args
        );
        let job_b = jobs.iter().find(|j| j.args.contains(&item_b)).unwrap();
        assert!(
            job_b.args.contains(&"/repo/b".to_string()),
            "job for proj-b's item must carry proj-b's own folder path, got {:?}",
            job_b.args
        );
    }

    fn seed_gate_label(mcp: &AgentflareMcp) -> std::collections::HashMap<String, String> {
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            let _ = agentflare_backend::label::create(
                conn,
                agentflare_backend::label::CreateLabel {
                    project_id: Some(project.id.clone()),
                    workspace_id: project.workspace_id.clone(),
                    name: NEEDS_HUMAN_GATE_LABEL.into(),
                    color: None,
                    parent_id: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                },
            );
            let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
            labels.into_iter().map(|l| (l.name, l.id)).collect()
        })
        .unwrap()
    }

    #[test]
    fn job_in_flight_detects_a_queued_job_for_the_item() {
        let queue = test_queue();
        let job = agentflare_jobs::AgentJob::new("agentflare-work")
            .args(["item-1".to_string(), "claude-code".to_string()])
            .in_process();
        queue.enqueue(&job).unwrap();

        assert!(job_in_flight(&queue, "item-1"));
        assert!(!job_in_flight(&queue, "item-2"));
    }

    #[test]
    fn run_review_sweep_ignores_items_not_in_review() {
        let mcp = test_mcp();
        let queue = test_queue();
        let _item_id = seed_ready_item(&mcp, Some("claude-code"));

        let auth_conn = test_auth_conn();
        let result = run_review_sweep(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(result.promoted, 0);
        assert_eq!(result.self_repaired, 0);
        assert_eq!(result.skipped, 0);
    }

    #[test]
    fn run_review_sweep_skips_an_item_whose_pr_status_cannot_be_determined() {
        let repo = throwaway_repo();
        let mcp = test_mcp_with_repo(repo.path().to_path_buf());
        let queue = test_queue();
        let _item_id = seed_in_review_item(&mcp, Some("claude-code"));

        let auth_conn = test_auth_conn();
        let result = run_review_sweep(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(result.promoted, 0);
        assert_eq!(result.self_repaired, 0);
        assert_eq!(result.skipped, 1);
        assert!(queue.list(None).unwrap().is_empty());
    }

    #[test]
    fn self_repair_or_gate_dispatches_a_job_and_posts_a_marker_comment() {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item(&mcp, Some("claude-code"));
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap();
        let label_id_by_name = seed_gate_label(&mcp);
        let auth_conn = test_auth_conn();

        let dispatched = self_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            &["clippy".to_string()],
            &label_id_by_name,
        );

        assert!(dispatched);
        let jobs = queue.list(None).unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].args.contains(&item_id));
        let comments = mcp
            .with_backend_db(|conn| {
                agentflare_backend::comment::list_by_item(conn, &item_id).unwrap()
            })
            .unwrap();
        assert!(
            comments
                .iter()
                .any(|c| c.body.starts_with(CI_SELF_REPAIR_MARKER))
        );
    }

    #[test]
    fn self_repair_or_gate_gates_instead_of_dispatching_once_the_cap_is_reached() {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item(&mcp, Some("claude-code"));
        let label_id_by_name = seed_gate_label(&mcp);
        let auth_conn = test_auth_conn();

        // Pre-seed SELF_REPAIR_CAP prior marker comments -- as if this many
        // repair rounds already ran with CI still red.
        for _ in 0..crate::quota::decide::SELF_REPAIR_CAP {
            mcp.comment_impl(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.clone()),
                body: Some(format!("{CI_SELF_REPAIR_MARKER}\n\njob: prior")),
                ..Default::default()
            })
            .unwrap();
        }
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap();

        let dispatched = self_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            &["clippy".to_string()],
            &label_id_by_name,
        );

        assert!(!dispatched);
        assert!(queue.list(None).unwrap().is_empty());
        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(labels_contain_name(&mcp, &labels, NEEDS_HUMAN_GATE_LABEL));
    }

    #[test]
    fn self_repair_or_gate_stays_quiet_once_already_gated() {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item(&mcp, Some("claude-code"));
        let label_id_by_name = seed_gate_label(&mcp);
        mcp.item_add_label(ItemRequest {
            action: "add_label".into(),
            id: Some(item_id.clone()),
            label_id: Some(label_id_by_name[NEEDS_HUMAN_GATE_LABEL].clone()),
            ..Default::default()
        })
        .unwrap();
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap();
        let auth_conn = test_auth_conn();

        let dispatched = self_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            &["clippy".to_string()],
            &label_id_by_name,
        );

        assert!(!dispatched);
        assert!(queue.list(None).unwrap().is_empty());
    }

    #[test]
    fn self_repair_or_gate_does_not_double_dispatch_while_a_job_is_already_in_flight() {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item(&mcp, Some("claude-code"));
        let label_id_by_name = seed_gate_label(&mcp);
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap();
        let auth_conn = test_auth_conn();
        let job = agentflare_jobs::AgentJob::new("agentflare-work")
            .args([item_id.clone(), "claude-code".to_string()])
            .in_process();
        queue.enqueue(&job).unwrap();

        let dispatched = self_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            &["clippy".to_string()],
            &label_id_by_name,
        );

        assert!(!dispatched);
        assert_eq!(
            queue.list(None).unwrap().len(),
            1,
            "must not enqueue a second job"
        );
    }
}
