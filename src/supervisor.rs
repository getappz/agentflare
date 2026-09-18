//! Background discovery loop: finds items labeled `ready-for-work` and
//! dispatches an in-process work job (`WorkItemExecutor`, running the same
//! logic as `agentflare work`) for each one whose assignee is a
//! confirmed-autonomous agent (skips the rest with a comment).

use crate::mcp_server::AgentflareMcp;
use crate::mcp_server::types::{CommentRequest, ItemRequest};

mod telegram;
pub(crate) use telegram::poll_telegram_approvals;
#[cfg(test)]
use telegram::{
    IN_FLIGHT_UPDATE_OFFSETS, handle_chat_message, handle_telegram_callback, mark_in_flight,
    parse_approve_callback, safe_offset_to_persist, settle,
};

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

/// Stage labels the review sweep swaps on the PR itself as it moves through
/// self-repair, mirroring the `agentflare:in-review` -> `agentflare:completed`
/// convention `worktree::push_and_open_pr`/`relabel_pr_completed` already use
/// for the outer create/merge stages -- these extend the same vocabulary for
/// what happens in between, so a human watching the PR on GitHub (rather
/// than the item's internal comment thread) can see it's being repaired
/// automatically instead of just silently sitting on red CI.
const IN_REVIEW_PR_LABEL: &str = "agentflare:in-review";
const SELF_REPAIR_PR_LABEL: &str = "agentflare:self-repair";
const NEEDS_HUMAN_PR_LABEL: &str = "agentflare:needs-human";
/// Same stage-label convention, for a CI-green PR whose CodeRabbit review
/// still has unresolved findings (item #273's `coderabbit_repair_or_gate`).
const CODERABBIT_REPAIR_PR_LABEL: &str = "agentflare:review-repair";

/// Best-effort GitHub-visible stage transition for a PR: removes `from` (if
/// any -- tolerates it already being absent, same as every other caller of
/// `remove_label`), adds `to`, and posts `comment`. Never returns an error:
/// a lost status update must never block or undo the sweep's own DB
/// mutation, which has already happened by the time this runs -- same
/// fail-open contract `relabel_pr_completed` and `merge_approved_pr` use for
/// their own GitHub calls.
fn update_pr_stage(folder_path: &str, number: u64, from: Option<&str>, to: &str, comment: &str) {
    let Some(repo) = crate::github::RepoId::resolve_from_remote(std::path::Path::new(folder_path))
    else {
        return;
    };
    let Ok(client) = crate::github::Client::new() else {
        return;
    };
    if let Err(e) = crate::github::issues::add_labels(&client, &repo, number, &[to.to_string()]) {
        eprintln!("agentflare-supervisor: could not add {to} to PR #{number}: {e}");
    }
    if let Some(from) = from
        && let Err(e) = crate::github::issues::remove_label(&client, &repo, number, from)
    {
        eprintln!("agentflare-supervisor: could not remove {from} from PR #{number}: {e}");
    }
    if let Err(e) = crate::github::issues::comment(&client, &repo, number, comment) {
        eprintln!("agentflare-supervisor: could not comment on PR #{number}: {e}");
    }
}

/// Whether `run_review_sweep` should dispatch an agent to rebase/resolve a
/// PR that GitHub reports as `Conflicting`, instead of just surfacing it to
/// a human (see `handle_pr_status`'s `Conflicting` arm). Defaults to `false`
/// -- unlike `Behind`'s clean fast-forward, resolving a real conflict means
/// rewriting someone else's diff, which is a judgment call worth an explicit
/// opt-in rather than a repo-wide default.
///
/// Resolution order mirrors `github::bridge::config`'s project settings:
/// `AGENTFLARE_AUTO_RESOLVE_CONFLICTS` env var, else `.agentflare/config.toml`'s
/// `[review_sweep].auto_resolve_conflicts` with the project-local file
/// (repo-scoped, so a project can opt out even when the user's home config
/// opts in globally) taking precedence over the user-home file (so one
/// global default doesn't require setting it in every project). A malformed
/// or unreadable config file falls back to `false` rather than failing the
/// sweep tick over one bad setting.
fn auto_resolve_conflicts_enabled(repo_root: &std::path::Path) -> bool {
    if let Ok(v) = std::env::var("AGENTFLARE_AUTO_RESOLVE_CONFLICTS") {
        return crate::github::bridge::config::truthy(&v);
    }
    let Ok(layers) =
        flare_git_core::config_loader::locate_and_parse(repo_root, Some(&crate::paths::home()))
    else {
        return false;
    };
    [
        layers.project_local.as_ref().map(|(_, v)| v),
        layers.user_home.as_ref().map(|(_, v)| v),
    ]
    .into_iter()
    .flatten()
    .find_map(|doc| {
        doc.get("review_sweep")?
            .get("auto_resolve_conflicts")?
            .as_bool()
    })
    .unwrap_or(false)
}

/// `vault` secret holding the Telegram chat id human-gate pings go to.
/// Reuses the same `channels`/`vault` path as `agentflare channel send`
/// rather than inventing a separate config store for one setting -- set it
/// with `agentflare vault set telegram_notify_chat_id <chat_id>` alongside
/// `telegram_bot_token` (see `channels::Platform::secret_name`). Also read by
/// `crate::chat_channel` to authorize which chat's free-text/slash-command
/// messages it acts on -- the same chat a PR-approval card would be sent to.
pub(crate) const TELEGRAM_NOTIFY_CHAT_ID_SECRET: &str = "telegram_notify_chat_id";

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

/// Falls back to `~/.agentflare/config.toml`'s `[router]` rules for an item
/// with no usable `assignee_agent` -- the same rules `agentflare work`
/// already consults when a human runs it against an unassigned item
/// (`cli::work::resolve_agent`), reused here rather than duplicated so the
/// two paths can't drift. Without this, `self_repair_or_gate` used to just
/// skip an unassigned item's failing PR forever, with no comment, no label,
/// no cap counting -- an item `discover_untracked_prs` creates for a
/// hand-opened PR always has `assignee_agent: None`, so its self-repair
/// silently never fired even with a `[router]` rule configured to auto-pick
/// an implementer (confirmed live on image-qc item #19 -- stuck for hours
/// with a red PR and no dispatch attempt of any kind).
///
/// `run_discovery_tick`'s own tier-5 eligibility check
/// (`quota::decide::decide`) has the identical gap for a plain
/// `ready-for-work` item with no assignee, deliberately NOT fixed here: that
/// check runs on every item on every tick and is documented side-effect-free,
/// while this call's `detect_all_with` shells out to probe installed agent
/// CLIs and `state::save` persists a rotation counter -- both fine for
/// self-repair's much rarer per-failing-PR cadence, not for a per-tick,
/// per-item hot path. Fixing that one needs its own design pass (cache
/// detection results, or move routing before/outside the pure decide()).
fn route_unassigned(item: &agentflare_backend::item::Item) -> Option<agent_registry::Agent> {
    let mut state = crate::state::load();
    let installed: Vec<agent_registry::Agent> = agent_registry::detect_all_with(
        agent_registry::REGISTRY,
        &mut state.version_cache,
        &agent_registry::RealVersionRunner,
    )
    .iter()
    .filter_map(|d| agent_registry::agent_by_name(d.id))
    .collect();
    let config = crate::cli::work::load_router_config();
    let agent = route_unassigned_with(item, &config, &installed, &mut state.router_rotation);
    crate::state::save(&state);
    agent
}

/// The pure decision core of `route_unassigned`, split out so a test can
/// drive it with a synthetic `config`/`installed`/`rotation` instead of this
/// machine's real installed-agent detection and `~/.agentflare/config.toml`.
fn route_unassigned_with(
    item: &agentflare_backend::item::Item,
    config: &agent_registry::RouterConfig,
    installed: &[agent_registry::Agent],
    rotation: &mut std::collections::HashMap<String, u64>,
) -> Option<agent_registry::Agent> {
    let task = agent_registry::TaskContext {
        labels: Vec::new(),
        kind: crate::mcp_server::item::parsed_kind(&item.metadata),
        size: crate::mcp_server::item::parsed_size(&item.metadata),
        repo: None,
        assigned_agent: None,
        role: Some("implementer".to_string()),
    };
    let autonomous: Vec<agent_registry::Agent> = installed
        .iter()
        .copied()
        .filter(|agent| agent_registry::autonomous_args(*agent).is_some())
        .collect();
    agent_registry::route(&task, config, &autonomous, rotation).map(|d| d.agent)
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
                    match dispatch_item(
                        mcp,
                        queue,
                        &item,
                        agent,
                        &folder_path,
                        &label_id_by_name,
                        &ready_id,
                    ) {
                        DispatchOutcome::Dispatched => result.dispatched += 1,
                        DispatchOutcome::WaitingOnPlan => result.waiting += 1,
                        DispatchOutcome::NotDispatched => {}
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

/// Records a supervisor decision on `item` -- the label swap plus the
/// comment humans and `dispatch_failure_ceiling` read -- directly against
/// the backend by the item's own canonical ids, never through the
/// `ItemRequest`/`CommentRequest` MCP entry points.
///
/// Those entry points resolve every id via `resolve_item_id`, which only
/// accepts items of the ONE project this daemon process is linked to (the
/// cwd it was started in), while `run_discovery_tick` deliberately walks
/// every registered `project_dirs` folder. For any other project's items
/// the swap and the comment therefore failed -- silently, since each call
/// was `let _ =` -- and a daemon started by the watchdog scheduled task
/// (cwd `C:\Windows\System32`) matched no real project at all. Visible
/// damage: not one `## supervisor — dispatched` marker was posted anywhere
/// after 2026-08-31, so `dispatch_failure_ceiling` counted zero cycles,
/// `handle_terminal_job_failure` put `ready-for-work` straight back, and a
/// deterministically failing item was re-dispatched every
/// `SUPERVISOR_DISCOVERY_INTERVAL` without bound (image-qc item #273: 1,367
/// jobs in 31 hours, 2026-09-16/17). A skipped item likewise never lost
/// `ready-for-work` and was re-skipped, with a fresh comment, every tick.
///
/// Failures are logged, not swallowed: a swap that didn't land is exactly
/// what re-arms the loop on the next tick (item #221).
fn record_supervisor_action(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    remove_label_id: Option<&str>,
    add_label_id: Option<&str>,
    comment: &str,
) {
    let author = crate::claims::owner_id();
    // One transaction: `with_backend_db` only locks and opens the connection,
    // it does not itself start one, so without this a failure partway through
    // (e.g. `add_label` after `remove_label` already ran) leaves the earlier
    // writes committed -- exactly the half-applied state ("ready-for-work
    // gone, no dispatched label, no marker comment") that lets an item slip
    // back into the discovery loop invisibly.
    let outcome = mcp.with_backend_db(|conn| -> agentflare_backend::error::Result<()> {
        let tx = conn.unchecked_transaction()?;
        if let Some(id) = remove_label_id {
            agentflare_backend::item::remove_label(&tx, &item.id, id)?;
        }
        if let Some(id) = add_label_id {
            agentflare_backend::item::add_label(&tx, &item.id, id)?;
        }
        agentflare_backend::comment::create(&tx, &item.id, &author, comment)?;
        tx.commit()?;
        Ok(())
    });
    let err = match outcome {
        Ok(Ok(())) => return,
        Ok(Err(e)) => e.to_string(),
        Err(e) => e.to_string(),
    };
    eprintln!(
        "agentflare-supervisor: failed to record supervisor action on item #{} ({}): {err}",
        item.sequence_id, item.id
    );
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
    record_supervisor_action(
        mcp,
        item,
        Some(ready_id),
        label_id_by_name.get(NEEDS_MANUAL_LABEL).map(String::as_str),
        &format!("## supervisor — skipped\n\n{reason}. Run `agentflare work` manually."),
    );
    if first_time_gated(&item.id) {
        notify_human_gate(item, &reason);
    }
}

fn ask_item(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    question: &str,
    label_id_by_name: &std::collections::HashMap<String, String>,
    ready_id: &str,
) {
    record_supervisor_action(
        mcp,
        item,
        Some(ready_id),
        label_id_by_name
            .get(NEEDS_HUMAN_GATE_LABEL)
            .map(String::as_str),
        &format!("## supervisor — gated\n\n{question}"),
    );
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
pub(crate) fn item_model_override(metadata: &str) -> Option<String> {
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

/// What one `dispatch_item` call did, for `run_discovery_tick`'s counters.
/// A plain `bool` couldn't distinguish "nothing to report" from "this item is
/// waiting on something and an operator should see it in the tick summary" --
/// the same reason `SelfRepairOutcome` exists for the review sweep.
enum DispatchOutcome {
    Dispatched,
    /// Blocked on an unapproved plan gate. Retryable by definition: the item
    /// keeps its `ready-for-work` label and the very next tick dispatches it
    /// once the plan is approved, so it belongs in
    /// `DiscoveryTickResult::waiting`, not `skipped`. Before item #573's final
    /// review this returned a bare `false` and was counted against NO counter
    /// at all, so an auto-gated `urgent`/`high` item could sit undispatched
    /// forever with nothing but a repeated stderr line to show for it.
    WaitingOnPlan,
    /// Nothing was enqueued and there is nothing for this tick to wait on: a
    /// job is already queued/running for the item, or the enqueue itself
    /// failed. Counted exactly as the prior `false` return was -- against no
    /// counter -- deliberately left unchanged here.
    NotDispatched,
}

fn dispatch_item(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    item: &agentflare_backend::item::Item,
    agent: agent_registry::Agent,
    folder_path: &str,
    label_id_by_name: &std::collections::HashMap<String, String>,
    ready_id: &str,
) -> DispatchOutcome {
    // Single-flight guard (item #221): a row already queued/running for this
    // item means a prior tick dispatched it — its ready→dispatched swap may
    // have failed, or reconcile re-armed it for auto-retry — and enqueueing
    // again just floods agent_jobs while workers never catch up. Skip; the
    // existing row will run.
    if job_in_flight(queue, &item.id) {
        eprintln!(
            "agentflare-supervisor: item #{} ({}) already has a queued/running job — skipping duplicate dispatch",
            item.sequence_id, item.id
        );
        return DispatchOutcome::NotDispatched;
    }
    if let agentflare_backend::item::PlanGateStatus::Blocked(status) =
        agentflare_backend::item::plan_gate::plan_gate_status(&item.metadata)
    {
        eprintln!(
            "agentflare-supervisor: item #{} ({}) blocked by plan gate (status: {status}) — skipping",
            item.sequence_id, item.id
        );
        // No plan has been submitted at all, so nothing else in the system
        // will ever ping a human about this item: `item_submit_plan` sends the
        // approve card, and it was never called. Since Task 7 auto-gates every
        // `urgent`/`high` item, that would otherwise leave them stalled
        // silently and indefinitely. `"pending"`/`"rejected"` already had
        // their notification at submit/reject time, so only `"none"` pings.
        // `first_time_gated` is the same once-per-item-per-process idiom
        // `run_discovery_tick` already uses for `NEEDS_DECISION_LABEL`, so
        // this fires once per gate rather than once per tick. Namespaced
        // ("plan:<id>", not the bare item id) because the underlying set is
        // keyed globally across every gate type in this file -- an
        // unnamespaced key here would consume the same token the PR-approval
        // card (`notify_pr_approval_gate`) checks for the same item later in
        // its life, silently suppressing that card for every auto-gated item.
        if status == "none" && first_time_gated(&format!("plan:{}", item.id)) {
            notify_human_gate(
                item,
                "auto-gated: needs a plan — call item(action=\"submit_plan\", plan_asset_id=...) \
                 before this item can be dispatched",
            );
        }
        return DispatchOutcome::WaitingOnPlan;
    }
    // Resolved before enqueueing, not merely at label-swap time: discovery
    // only requires `READY_LABEL` to exist on a project (see
    // `run_discovery_tick`), so a project that never got `DISPATCHED_LABEL`
    // seeded would otherwise enqueue a real job, remove `ready-for-work`, and
    // leave the item wearing neither label -- invisible to the dashboard and
    // to the next discovery query, with no way back onto `ready-for-work`
    // short of a human relabeling it by hand.
    let Some(dispatched_id) = label_id_by_name.get(DISPATCHED_LABEL) else {
        eprintln!(
            "agentflare-supervisor: item #{} ({}) not dispatched — project has no {DISPATCHED_LABEL} label",
            item.sequence_id, item.id
        );
        return DispatchOutcome::NotDispatched;
    };
    let Some(info) = enqueue_work_job(queue, item, agent, Some(folder_path), None) else {
        return DispatchOutcome::NotDispatched;
    };

    // ready-for-work -> dispatched, plus the one-per-cycle dispatch marker
    // `dispatch_failure_ceiling` counts. Done by canonical id straight
    // against the backend (see `record_supervisor_action`): the item may
    // belong to any registered project, not just the one this daemon's
    // cwd is linked to, and a swap that silently doesn't land leaves the
    // item visible to the next tick's discovery query, re-arming the loop
    // above (item #221).
    record_supervisor_action(
        mcp,
        item,
        Some(ready_id),
        Some(dispatched_id.as_str()),
        &format!(
            "{}\n\njob: {}",
            crate::dispatch_failure_ceiling::DISPATCH_MARKER,
            info.id
        ),
    );
    DispatchOutcome::Dispatched
}

/// Marker prefix on a self-repair-dispatch comment (see `self_repair_item`
/// below) -- `run_review_sweep` counts these on an item to enforce
/// `quota::decide::SELF_REPAIR_CAP` without a separate persistent counter,
/// the same way an item's `metadata` isn't otherwise touched by this file.
const CI_SELF_REPAIR_MARKER: &str = "## supervisor — CI self-repair dispatched";
/// Same role as `CI_SELF_REPAIR_MARKER`, kept as its own prefix (rather than
/// reusing it) so `self_repair_or_gate`'s `prior_attempts`/cap count for a
/// CI-failure dispatch and a merge-conflict dispatch independently -- a PR
/// that burned its CI-repair cap must still get a fresh conflict-repair
/// attempt, and vice versa.
const CONFLICT_REPAIR_MARKER: &str = "## supervisor — merge-conflict repair dispatched";

/// What triggered `self_repair_or_gate` -- lets one dispatch/cap/claim/
/// cooldown/host-policy/routing implementation serve both the pre-existing
/// CI self-repair path and the new merge-conflict repair path
/// (`handle_pr_status`'s `Conflicting` arm) without duplicating it.
enum RepairTrigger<'a> {
    FailingChecks(&'a [String]),
    MergeConflict,
}

impl RepairTrigger<'_> {
    fn marker(&self) -> &'static str {
        match self {
            Self::FailingChecks(_) => CI_SELF_REPAIR_MARKER,
            Self::MergeConflict => CONFLICT_REPAIR_MARKER,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Self::FailingChecks(_) => "CI self-repair",
            Self::MergeConflict => "merge-conflict repair",
        }
    }

    /// What's actually wrong, for the comment/notify bodies.
    fn what(&self) -> String {
        match self {
            Self::FailingChecks(checks) => format!("Failing checks: {}", checks.join(", ")),
            Self::MergeConflict => {
                "This PR conflicts with its base branch and can't be merged automatically".into()
            }
        }
    }

    fn instruction(&self) -> &'static str {
        match self {
            Self::FailingChecks(_) => "Please investigate and push a fix.",
            Self::MergeConflict => {
                "Please rebase onto (or merge in) the base branch and resolve the conflicts."
            }
        }
    }

    fn cap_outcome(&self) -> &'static str {
        match self {
            Self::FailingChecks(_) => "no green build",
            Self::MergeConflict => "still conflicting",
        }
    }

    /// Short tag for the job's `dispatch_reason` -- `self-repair: <checks>`
    /// is an existing, tested string (`self_repair_or_gate_dispatches_a_job_and_posts_a_marker_comment`);
    /// kept unchanged rather than routed through `what()`'s longer prose.
    fn dispatch_reason(&self) -> String {
        match self {
            Self::FailingChecks(checks) => format!("self-repair: {}", checks.join(", ")),
            Self::MergeConflict => "conflict-repair: merge conflict with base branch".into(),
        }
    }
}

/// Marker prefix on a CodeRabbit-review-repair-dispatch comment (see
/// `coderabbit_repair_or_gate` below) -- counted the same way
/// `CI_SELF_REPAIR_MARKER` is, against the same `quota::decide::SELF_REPAIR_CAP`,
/// so a PR CodeRabbit keeps flagging doesn't retry-dispatch forever either.
const CODERABBIT_REPAIR_MARKER: &str = "## supervisor — CodeRabbit review repair dispatched";

/// Login prefix every known CodeRabbit bot account posts review comments
/// under (`coderabbitai[bot]` today) -- matched as a prefix rather than an
/// exact string so a renamed/enterprise variant of the same bot isn't
/// silently invisible to `unresolved_coderabbit_comments`.
const CODERABBIT_LOGIN_PREFIX: &str = "coderabbit";

pub(crate) struct ReviewSweepResult {
    pub promoted: usize,
    pub self_repaired: usize,
    /// PRs whose CodeRabbit review left unresolved findings and got a
    /// capped review-repair job dispatched for them -- mirrors
    /// `self_repaired`, but reacting to CodeRabbit's own review threads
    /// rather than CI (item #273: `run_review_sweep` previously only ever
    /// looked at CI check status and `mergeable_state`, never the review
    /// itself).
    pub review_repaired: usize,
    pub skipped: usize,
    /// Items a later sweep should retry (agent cooling down, or the host
    /// resource gate throttling/pausing dispatch) rather than ones this
    /// sweep decided against. Mirrors `DiscoveryTickResult::waiting` — a
    /// deferral counted as "skipped" reads to an operator as a decision
    /// that won't be revisited, which is exactly backwards (item #82).
    pub waiting: usize,
    /// PRs cleanly behind the base branch (GitHub's own `mergeable_state`)
    /// that this sweep brought up to date via `pulls::update_branch` (item
    /// #197's follow-up) -- distinct from `promoted`/`self_repaired` since
    /// neither the item's state nor its CI outcome changed, just its branch
    /// content; the next tick re-evaluates it against fresh CI.
    pub updated: usize,
    /// Trusted-author PRs found with no item tracking them yet, each just
    /// given a synthesized `in_review` item via `worktree::discover_untracked_prs`
    /// -- distinct from every other counter since nothing about a PR's own
    /// state changed, only whether this sweep can see it; the newly created
    /// item is picked up by the *next* tick's normal per-item loop, not this
    /// one.
    pub discovered: usize,
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
/// same shape `ProjectBatch` gives `run_discovery_tick`. `project_id`,
/// `in_review_state_id`, and `known_pr_numbers` exist only to let
/// `discover_untracked_prs` create a correctly-scoped item directly (item
/// creation needs an explicit project, unlike every other mutation here,
/// which is addressed by an existing item's own id).
struct ReviewBatch {
    folder_path: String,
    project_id: String,
    in_review_state_id: String,
    known_pr_numbers: std::collections::HashSet<u64>,
    items: Vec<agentflare_backend::item::Item>,
    label_id_by_name: std::collections::HashMap<String, String>,
    /// Items #234's self-heal: items that regressed out of "in_review" (e.g.
    /// an orphaned self-repair job's claim getting reconciled) while still
    /// carrying a tracked, untouched `metadata.pr.number`, with no live
    /// claim and no lifecycle gate (`NEEDS_MANUAL_LABEL`/
    /// `NEEDS_DECISION_LABEL`) holding them back deliberately. Not yet
    /// restored to "in_review" here -- see `stray_pr_is_still_relevant`'s
    /// doc comment for why that has to wait until a live GitHub PR check.
    stray_candidates: Vec<(agentflare_backend::item::Item, u64)>,
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
        review_repaired: 0,
        skipped: 0,
        waiting: 0,
        updated: 0,
        discovered: 0,
    };
    // Computed once, not per-project/per-PR: identifies this workstation to
    // `claim_pr_for_discovery`'s marker comment so two workstations racing to
    // discover the same PR can tell each other apart. Same persisted id
    // `github::bridge` itself uses.
    let discovery_owner = crate::github::bridge::config::stable_instance_id();

    let fetched = mcp.with_backend_db(|conn| {
        let dirs = agentflare_backend::project_dir::list(conn).ok()?;
        let mut batches = Vec::new();
        for dir in dirs {
            let items = agentflare_backend::item::list_by_project(conn, &dir.project_id).ok()?;
            let states = agentflare_backend::state::list_by_project(conn, &dir.project_id).ok()?;
            let state_by_id: std::collections::HashMap<&str, &agentflare_backend::state::State> =
                states.iter().map(|s| (s.id.as_str(), s)).collect();
            // Every item, not just in_review ones -- an item could already be
            // tracking a PR from `started` (agent still working) or any other
            // state, and `discover_untracked_prs` must never create a second
            // item for a PR one of those already owns.
            let known_pr_numbers = crate::worktree::tracked_pr_numbers(&items);
            let Some(in_review_state_id) = states
                .iter()
                .find(|s| s.group_name == "in_review")
                .map(|s| s.id.clone())
            else {
                // No in_review state group in this project at all -- nothing
                // for this sweep to do here regardless of discovery.
                continue;
            };
            let in_review: Vec<_> = items
                .iter()
                .filter(|i| {
                    state_by_id
                        .get(i.state_id.as_str())
                        .is_some_and(|s| s.group_name == "in_review")
                })
                .cloned()
                .collect();
            let labels = agentflare_backend::label::list_by_project(conn, &dir.project_id).ok()?;
            let mut label_id_by_name = std::collections::HashMap::new();
            for l in &labels {
                label_id_by_name.insert(l.name.clone(), l.id.clone());
            }
            // Self-heal items #234: an item can regress out of the
            // "in_review" group (e.g. a self-repair job's claim getting
            // reconciled as orphaned/failed) while its PR is still open and
            // tracked -- once that happens, this sweep's query above can
            // never see it again, even after the PR merges on GitHub (item
            // #233 sat stuck in "Backlog" for hours after its PR merged).
            // Collect candidates for recovery here; whether each one
            // actually gets restored to "in_review" is decided once a repo
            // and GitHub client are available below (`stray_pr_is_still_relevant`),
            // never on metadata presence alone -- a first attempt at this
            // fix restored on metadata + claim-liveness only, which also
            // silently overrode `NEEDS_MANUAL_LABEL`'s dispatch-failure cap
            // and `NEEDS_DECISION_LABEL`'s go/no-go gate (review finding on
            // item #234's first attempt), so both are excluded here too.
            let now = crate::claims::now();
            let requested_ttl = crate::mcp_server::types::backend_claim_ttl_secs();
            let mut stray_candidates = Vec::new();
            for item in &items {
                if state_by_id.get(item.state_id.as_str()).is_some_and(|s| {
                    matches!(
                        s.group_name.as_str(),
                        "in_review" | "completed" | "cancelled"
                    )
                }) {
                    continue;
                }
                let Some(number) = crate::worktree::pr_number_from_metadata(item) else {
                    continue;
                };
                let item_label_ids = agentflare_backend::item::list_labels(conn, &item.id)
                    .ok()
                    .unwrap_or_default();
                let gated = [NEEDS_MANUAL_LABEL, NEEDS_DECISION_LABEL]
                    .iter()
                    .any(|name| {
                        label_id_by_name
                            .get(*name)
                            .is_some_and(|id| item_label_ids.contains(id))
                    });
                if gated {
                    continue;
                }
                let ttl =
                    agentflare_backend::claim::effective_ttl_secs(conn, &item.id, requested_ttl);
                let has_live_claim =
                    agentflare_backend::claim::live_claim_on_item(conn, &item.id, now, ttl)
                        .ok()
                        .flatten()
                        .is_some();
                if has_live_claim {
                    continue;
                }
                stray_candidates.push((item.clone(), number));
            }
            batches.push(ReviewBatch {
                folder_path: dir.folder_path,
                project_id: dir.project_id,
                in_review_state_id,
                known_pr_numbers,
                items: in_review,
                label_id_by_name,
                stray_candidates,
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
            project_id,
            in_review_state_id,
            known_pr_numbers,
            mut items,
            label_id_by_name,
            stray_candidates,
        } = batch;
        let repo_root = std::path::PathBuf::from(&folder_path);
        // Resolved once per project and reused for discovery, the batched
        // GraphQL fetch below, and (implicitly, inside `pr_ci_status`) the
        // per-item REST fallback -- rather than every one of those re-doing
        // the same remote/credential resolution, as the old one-call-per-item
        // loop used to via its own internal `pr_ci_status` call.
        let resolved = (
            crate::github::RepoId::resolve_from_remote(&repo_root),
            crate::github::Client::new(),
        );
        // Item #234 self-heal, continued: only now, with a resolved repo and
        // an authenticated client in hand, do stray candidates actually get
        // restored -- and only the ones whose PR a live GitHub call confirms
        // is still open or already merged. No remote/no credentials means no
        // way to tell "orphaned mid-repair" apart from "PR was closed
        // without merging", so this soft-fails exactly like every other
        // GitHub-touching branch in this sweep: skip this tick, the item
        // stays where it is, and it's retried on the next one.
        if let (Some(repo), Ok(client)) = &resolved {
            for (item, number) in &stray_candidates {
                if !stray_pr_is_still_relevant(client, repo, *number) {
                    continue;
                }
                let restored = mcp.with_backend_db(|conn| {
                    agentflare_backend::item::update_state(conn, &item.id, &in_review_state_id)
                });
                if let Ok(Ok(restored)) = restored {
                    items.push(restored);
                }
            }
        }
        if let (Some(repo), Ok(client)) = &resolved {
            let discovered = mcp
                .with_backend_db(|conn| {
                    crate::worktree::discover_untracked_prs(
                        conn,
                        client,
                        repo,
                        &project_id,
                        &in_review_state_id,
                        &known_pr_numbers,
                        &discovery_owner,
                    )
                })
                .unwrap_or(0);
            result.discovered += discovered;
        }

        // Items carrying `metadata.pr.number` (set by `push_and_open_pr` at
        // PR-creation time) are batched into a handful of GraphQL queries
        // instead of one REST call each -- see `github::graphql`'s doc
        // comment for the rate-limit math this avoids. Only items that
        // predate that field fall back to the old one-REST-call-per-item
        // path below, which also carries the branch-name-heuristic lookup
        // those items still need.
        let mut numbered: Vec<(&agentflare_backend::item::Item, u64)> = Vec::new();
        let mut unnumbered: Vec<&agentflare_backend::item::Item> = Vec::new();
        for item in &items {
            match crate::worktree::pr_number_from_metadata(item) {
                Some(number) => numbered.push((item, number)),
                None => unnumbered.push(item),
            }
        }

        let batch_data = if let (Some(repo), Ok(client)) = &resolved {
            let numbers: Vec<u64> = numbered.iter().map(|(_, n)| *n).collect();
            crate::github::graphql::batch_pr_status_chunked(client, repo, &numbers)
        } else {
            std::collections::HashMap::new()
        };

        for (item, number) in numbered {
            // A PR number missing from `batch_data` (query failed for its
            // chunk, or GitHub couldn't resolve that PR) is treated exactly
            // like any other soft-fail: `Unknown`, polled again next tick --
            // never an error for the whole sweep.
            let status = match batch_data.get(&number) {
                Some(data) => crate::worktree::pr_ci_status_from_batch(number, data),
                None => crate::worktree::PrCiStatus::Unknown,
            };
            handle_pr_status(
                mcp,
                queue,
                auth_conn,
                host_policy,
                item,
                status,
                &label_id_by_name,
                &folder_path,
                &repo_root,
                &mut result,
            );
        }
        for item in unnumbered {
            let status = crate::worktree::pr_ci_status(item, &repo_root);
            handle_pr_status(
                mcp,
                queue,
                auth_conn,
                host_policy,
                item,
                status,
                &label_id_by_name,
                &folder_path,
                &repo_root,
                &mut result,
            );
        }
    }
    result
}

/// Acts on one item's already-fetched `PrCiStatus`, however it was fetched --
/// batched via GraphQL or singly via REST. Split out of `run_review_sweep`'s
/// loop so both fetch paths (`numbered`/`unnumbered` above) drive the exact
/// same decision-and-mutate logic instead of two copies that could drift.
///
/// Every mutating branch here (`promote_merged_item`, `merge_if_approved`,
/// `update_stale_branch`) makes its own live GitHub call as the actual
/// authority, regardless of how stale `status` (a point-in-time snapshot,
/// batched or not) might be by the time this runs -- a rejection from that
/// live call (already merged, already up to date, no longer mergeable) falls
/// through to `skipped` rather than erroring, so acting on a stale snapshot
/// is always safe. `self_repair_or_gate` additionally re-checks claim
/// liveness before dispatching, guarding the one branch here that starts new
/// work rather than just re-attempting an idempotent GitHub operation.
#[allow(clippy::too_many_arguments)]
fn handle_pr_status(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    auth_conn: &rusqlite::Connection,
    host_policy: agentflare_resource_gate::Policy,
    item: &agentflare_backend::item::Item,
    status: crate::worktree::PrCiStatus,
    label_id_by_name: &std::collections::HashMap<String, String>,
    folder_path: &str,
    repo_root: &std::path::Path,
    result: &mut ReviewSweepResult,
) {
    match status {
        crate::worktree::PrCiStatus::Merged => {
            if promote_merged_item(mcp, item) {
                result.promoted += 1;
            } else {
                result.skipped += 1;
            }
        }
        crate::worktree::PrCiStatus::Failing {
            number,
            checks,
            labels,
        } => {
            match self_repair_or_gate(
                mcp,
                queue,
                auth_conn,
                host_policy,
                item,
                number,
                RepairTrigger::FailingChecks(&checks),
                &labels,
                label_id_by_name,
                folder_path,
            ) {
                SelfRepairOutcome::Dispatched => result.self_repaired += 1,
                SelfRepairOutcome::Deferred => result.waiting += 1,
                SelfRepairOutcome::Skipped => result.skipped += 1,
            }
        }
        crate::worktree::PrCiStatus::Passing { number, labels } => {
            // CI just went green -- if the PR was still carrying a
            // self-repair/needs-human stage label from before, swap it back
            // to plain in-review rather than leaving a stale "under repair"
            // label on a now-passing PR. `labels` is already in hand from
            // the batched/single fetch above, so this only touches GitHub
            // when there's actually something to revert.
            if let Some(stale) = [SELF_REPAIR_PR_LABEL, NEEDS_HUMAN_PR_LABEL]
                .into_iter()
                .find(|l| labels.iter().any(|have| have == l))
            {
                update_pr_stage(
                    folder_path,
                    number,
                    Some(stale),
                    IN_REVIEW_PR_LABEL,
                    "## supervisor — CI green\n\nChecks are passing again.",
                );
            }
            // Namespaced ("pr-approval:<id>", not the bare item id): the
            // underlying set is keyed globally across every gate type in
            // this file (see `dispatch_item`'s "plan:" comment) -- an
            // unnamespaced key here silently starves this card of its
            // once-per-gate notify if the item was already gated for an
            // unrelated reason earlier in its life (e.g. the go/no-go
            // decision gate below, or `skip_item`), since that gate's call
            // already consumed the bare-id token (item #587).
            if !labels.iter().any(|l| l == PR_APPROVAL_LABEL)
                && first_time_gated(&format!("pr-approval:{}", item.id))
            {
                notify_pr_approval_gate(item, folder_path, number);
            }
            if merge_if_approved(mcp, item, repo_root, number, &labels) {
                result.promoted += 1;
            } else {
                // Not merged (no approval yet, or the merge call itself was
                // rejected) -- CI being green doesn't mean the PR is actually
                // done if CodeRabbit's own review still has unresolved
                // findings sitting on it untouched (item #273). Skip the two
                // live GitHub calls this fetch costs entirely once the item
                // is already gated or a repair job is already in flight --
                // `coderabbit_repair_or_gate` would just discard the findings
                // and return `Skipped` anyway, but not before paying for the
                // fetch on every single tick for as long as the PR sits
                // gated or in-flight.
                if already_gated_or_in_flight(mcp, queue, item, label_id_by_name) {
                    result.skipped += 1;
                } else {
                    let findings = fetch_unresolved_coderabbit_comments(repo_root, number);
                    match coderabbit_repair_or_gate(
                        mcp,
                        queue,
                        auth_conn,
                        host_policy,
                        item,
                        number,
                        &findings,
                        &labels,
                        label_id_by_name,
                        folder_path,
                    ) {
                        SelfRepairOutcome::Dispatched => result.review_repaired += 1,
                        SelfRepairOutcome::Deferred => result.waiting += 1,
                        SelfRepairOutcome::Skipped => result.skipped += 1,
                    }
                }
            }
        }
        crate::worktree::PrCiStatus::Behind { number } => {
            if crate::worktree::update_stale_branch(repo_root, number) {
                result.updated += 1;
            } else {
                result.skipped += 1;
            }
        }
        crate::worktree::PrCiStatus::Conflicting { number } => {
            if auto_resolve_conflicts_enabled(repo_root) {
                match self_repair_or_gate(
                    mcp,
                    queue,
                    auth_conn,
                    host_policy,
                    item,
                    number,
                    RepairTrigger::MergeConflict,
                    // No PR labels in hand here -- `Conflicting` is detected
                    // before the label fetch that `Failing`/`Passing` carry
                    // (same reasoning as skipping the check-run fetch). Worst
                    // case a stale self-repair/review-repair stage label
                    // isn't cleared before this one is added.
                    &[],
                    label_id_by_name,
                    folder_path,
                ) {
                    SelfRepairOutcome::Dispatched => result.self_repaired += 1,
                    SelfRepairOutcome::Deferred => result.waiting += 1,
                    SelfRepairOutcome::Skipped => result.skipped += 1,
                }
            } else {
                // Opt-out is the default (see `auto_resolve_conflicts_enabled`) --
                // surface it once instead of letting `merge_if_approved`'s live
                // merge call keep silently rejecting the same conflict every
                // tick forever.
                notify_conflict_gate(item, folder_path, number);
                result.skipped += 1;
            }
        }
        crate::worktree::PrCiStatus::Pending | crate::worktree::PrCiStatus::Unknown => {
            result.skipped += 1;
        }
    }
}

/// Item #234's self-heal (`run_review_sweep`'s `stray_candidates` handling):
/// confirms a stray item's tracked PR hasn't simply been closed without
/// merging before restoring the item to "in_review" -- an abandoned PR is
/// the one case metadata presence and claim liveness alone can't rule out.
/// Split out from the sweep's loop, mirroring `merge_approved_pr`'s own
/// test seam, so tests can drive it against a mock server instead of
/// `Client::new()`'s real credentials/host -- `run_review_sweep`'s own
/// integration tests never touch the network at all (see
/// `throwaway_repo`'s doc comment), so this is the only way to pin the
/// actual open/merged/closed decision.
fn stray_pr_is_still_relevant(
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    number: u64,
) -> bool {
    match crate::github::pulls::get(client, repo, number) {
        Ok(pr) => pr.state != "closed" || pr.merged_at.is_some(),
        Err(_) => false,
    }
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
/// `run_discovery_tick` only dispatches items with a resolvable
/// `assignee_agent`, so a dependent with none would just sit inert once
/// labeled -- a dependent with no assignee inherits the just-completed
/// item's own assignee (the agent that finished the blocking work is a
/// reasonable default owner for what it unblocked) before being labeled.
/// Only skipped, loudly, when the completed item itself has no assignee to
/// inherit from.
///
/// Idempotent and safe under concurrent sibling completions:
/// `item::add_label`'s `INSERT OR IGNORE` makes re-labeling a no-op, and an
/// already-`dispatched` item won't be relabeled `ready-for-work` by this
/// (it only ever adds the ready label, never touches `dispatched`).
pub(crate) fn cascade_unblock_dependents(conn: &rusqlite::Connection, item_id: &str) {
    let Ok(dependents) = agentflare_backend::item::dependents_of(conn, item_id) else {
        return;
    };
    if dependents.is_empty() {
        return;
    }
    let Ok(completed) = agentflare_backend::item::get(conn, item_id) else {
        return;
    };
    let inherited_assignee = completed
        .assignee_agent
        .as_deref()
        .map(agentflare_backend::item::agent_part);
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
            let Some(agent) = &inherited_assignee else {
                // The just-completed item has no assignee of its own to hand
                // off, so there's nothing sensible to inherit -- leave this
                // loudly unassigned rather than silently no-op'ing.
                eprintln!(
                    "agentflare-supervisor: item #{} ({}) has all dependencies completed but no assignee_agent to inherit (completed item {item_id} has none either) -- not auto-labeled {READY_LABEL}, needs manual dispatch",
                    dependent.sequence_id, dependent.id
                );
                continue;
            };
            if let Err(e) = agentflare_backend::item::update(
                conn,
                &dependent.id,
                agentflare_backend::item::UpdateItem {
                    assignee_agent: Some(agent.clone()),
                    ..Default::default()
                },
            ) {
                eprintln!(
                    "agentflare-supervisor: failed to inherit assignee {agent} onto item #{} ({}) after dependency {item_id} completed: {e}",
                    dependent.sequence_id, dependent.id
                );
                continue;
            }
        }
        let Ok(labels) = agentflare_backend::label::list_by_project(conn, &dependent.project_id)
        else {
            continue;
        };
        let Some(ready_id) = labels
            .into_iter()
            .find(|l| l.name == READY_LABEL)
            .map(|l| l.id)
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

/// Telegram notifications and the inbound channel-approval poll. Split out
/// when item #573's plan-gate work pushed this file past the LOC gate; glob
/// re-exported so every existing `crate::supervisor::notify_*` /
/// `first_time_gated` path (and `supervisor_tests.rs`'s `use super::*`)
/// keeps working unchanged.
pub(crate) mod notify;
pub(crate) use notify::*;

/// Whether `item` is already gated for a human (`NEEDS_HUMAN_GATE_LABEL`) or
/// has an `agentflare-work` job already queued/running -- the short-circuit
/// both `self_repair_or_gate` and `coderabbit_repair_or_gate` open with, and
/// what `handle_pr_status` checks up front before paying for a live
/// CodeRabbit-findings fetch it would otherwise throw away immediately (item
/// #273 follow-up: that fetch used to run unconditionally on every tick for
/// as long as a PR sat gated or in-flight).
fn already_gated_or_in_flight(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    item: &agentflare_backend::item::Item,
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
    already_gated || job_in_flight(queue, &item.id)
}

/// Which in-progress PR stage label a self-repair dispatch should remove
/// before adding `SELF_REPAIR_PR_LABEL` -- whichever of the known stage
/// labels the PR is currently carrying. CI self-repair and item #273's
/// CodeRabbit review-repair run against the same PR at different times, so a
/// PR heading into self-repair isn't always leaving plain in-review -- it
/// might be leaving `CODERABBIT_REPAIR_PR_LABEL` instead. Without checking
/// for that, a PR whose CI broke while under review-repair ended up with
/// both labels stacked, since removing a label that isn't there is a
/// silent no-op.
fn stale_stage_label(labels: &[String]) -> Option<&'static str> {
    [IN_REVIEW_PR_LABEL, CODERABBIT_REPAIR_PR_LABEL]
        .into_iter()
        .find(|l| labels.iter().any(|have| have == l))
}

/// Escape the characters Telegram's HTML `parse_mode` treats specially, so
/// an arbitrary item title/description can't break card formatting (or be
/// interpreted as an unintended tag).
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Telegram-only rich variant of [`notify_human_gate`] for the one gate a
/// human can resolve with a single tap: CI is green and the only thing
/// missing is `PR_APPROVAL_LABEL`. Unlike the plain-text pings, this carries
/// an inline "Approve" button whose `callback_data` embeds the repo and PR
/// number directly (`approve:{owner}/{repo}#{number}`) -- self-contained,
/// so [`poll_telegram_approvals`] never needs to re-resolve a worktree path
/// to act on a click. Same fail-open contract as `notify_human_gate`: no-ops
/// without a configured chat id or a resolvable repo, and a send failure
/// only logs.
fn notify_pr_approval_gate(item: &agentflare_backend::item::Item, folder_path: &str, number: u64) {
    let Ok(Some(chat_id)) = crate::vault::get_secret(TELEGRAM_NOTIFY_CHAT_ID_SECRET) else {
        return;
    };
    let Some(repo) = crate::github::RepoId::resolve_from_remote(std::path::Path::new(folder_path))
    else {
        return;
    };
    let excerpt: String = item.description.chars().take(200).collect();
    let text = format!(
        "\u{1F514} <b>agentflare</b> needs a human\n\
         <b>Repo:</b> {repo}\n\
         <b>Item:</b> #{} \u{2014} {}\n\
         {}\n\n\
         PR <a href=\"https://github.com/{repo}/pull/{number}\">#{number}</a> is CI-green and \
         mergeable, awaiting <code>{PR_APPROVAL_LABEL}</code>.",
        item.sequence_id,
        html_escape(&item.name),
        html_escape(&excerpt),
    );
    let callback_data = format!("approve:{repo}#{number}");
    if let Err(e) = crate::channels::send_telegram_card(
        &chat_id,
        &text,
        &[("\u{2705} Approve", &callback_data)],
    ) {
        eprintln!(
            "agentflare-supervisor: telegram card notify failed for item #{}: {e}",
            item.sequence_id
        );
    }
}

/// One-shot per item (namespaced separately from `notify_pr_approval_gate`'s
/// plain `item.id` key in `first_time_gated` -- the same item can hit both
/// gates at different points in its life and each must fire once on its
/// own): posts a GitHub-visible comment, swaps the PR's stage label to
/// `NEEDS_HUMAN_PR_LABEL`, and pings Telegram the first time this sweep sees
/// a real merge conflict on it while `auto_resolve_conflicts_enabled` is
/// off. Without this, `handle_pr_status`'s `Conflicting` arm would just
/// silently `skip` the item forever -- unlike `Passing`'s missing-approval
/// gate, nothing else in the sweep would ever surface it.
fn notify_conflict_gate(item: &agentflare_backend::item::Item, folder_path: &str, number: u64) {
    if !first_time_gated(&format!("conflict:{}", item.id)) {
        return;
    }
    let message = "## supervisor — merge conflict\n\n\
         This PR now conflicts with its base branch and can't be merged automatically. \
         Rebase or merge the base branch in and resolve the conflicts, then push.\n\n\
         (Set `auto_resolve_conflicts = true` under `[review_sweep]` in `.agentflare/config.toml` \
         -- project or user-home -- to have an agent attempt this automatically instead.)";
    update_pr_stage(
        folder_path,
        number,
        Some(IN_REVIEW_PR_LABEL),
        NEEDS_HUMAN_PR_LABEL,
        message,
    );
    notify_human_gate(
        item,
        &format!("PR #{number} has a merge conflict with its base branch"),
    );
}

/// True the first time a given item id is seen gated since this process
/// started, false on every later call for the same id -- `run_discovery_tick`
/// re-visits an already-gated item on every tick (it stays in the
/// `ready-for-work` query until a human clears `NEEDS_DECISION_LABEL`), so
/// this keeps `notify_human_gate` firing once per gate instead of once per
/// tick. In-memory and per-process by design: a daemon restart re-notifies
/// once, which is preferable to a persistent marker for a one-line ping.
pub(crate) fn first_time_gated(item_id: &str) -> bool {
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
    pr_number: u64,
    trigger: RepairTrigger<'_>,
    labels: &[String],
    label_id_by_name: &std::collections::HashMap<String, String>,
    folder_path: &str,
) -> SelfRepairOutcome {
    if already_gated_or_in_flight(mcp, queue, item, label_id_by_name) {
        return SelfRepairOutcome::Skipped;
    }

    let prior_attempts = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item.id))
        .ok()
        .and_then(Result::ok)
        .map(|comments| {
            comments
                .iter()
                .filter(|c| c.body.starts_with(trigger.marker()))
                .count() as u32
        })
        .unwrap_or(0);

    if prior_attempts >= crate::quota::decide::SELF_REPAIR_CAP {
        let cap_message = format!(
            "## supervisor — {} cap reached\n\n{}. \
             {} automatic repair attempt(s) already made, {} — needs a human look.",
            trigger.label(),
            trigger.what(),
            crate::quota::decide::SELF_REPAIR_CAP,
            trigger.cap_outcome(),
        );
        let _ = mcp.comment_impl(CommentRequest {
            action: "create".into(),
            item_id: Some(item.id.clone()),
            body: Some(cap_message.clone()),
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
        update_pr_stage(
            folder_path,
            pr_number,
            Some(SELF_REPAIR_PR_LABEL),
            NEEDS_HUMAN_PR_LABEL,
            &cap_message,
        );
        notify_human_gate(
            item,
            &format!(
                "{} cap reached ({} attempt(s), {})",
                trigger.label(),
                crate::quota::decide::SELF_REPAIR_CAP,
                trigger.what(),
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

    // Item #261: `claim_still_live` above only ever sees claims recorded in
    // THIS workstation's own local backend db -- per-workstation dbs are
    // never synced (see `worktree::discovery`'s doc comment) -- so it cannot
    // detect that a DIFFERENT workstation's `run_review_sweep` tick already
    // dispatched self-repair for this same PR. GitHub is the one medium
    // every workstation can see, so cross-machine coordination is arbitrated
    // there instead, via a claim marker comment on the PR and the same
    // TTL+heartbeat resolution `github::bridge::claim` already uses for
    // issue claiming (see `claim_self_repair`). Live incident: two
    // workstations independently self-repaired the same PR with no way to
    // see each other, each opened/labeled it as if it were the first,
    // leaving PR #688 with two different `beacon:` labels.
    let repo_and_client =
        crate::github::RepoId::resolve_from_remote(std::path::Path::new(folder_path))
            .zip(crate::github::Client::new().ok());
    if let Some((repo, client)) = repo_and_client
        && !claim_self_repair(&client, &repo, pr_number, &item.id, crate::claims::now())
    {
        return SelfRepairOutcome::Deferred;
    }

    let Some(agent) = item
        .assignee_agent
        .as_deref()
        .and_then(resolve_confirmed_agent)
        .or_else(|| route_unassigned(item))
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
    let reason = trigger.dispatch_reason();
    let Some(info) = enqueue_work_job(queue, item, agent, Some(folder_path), Some(&reason)) else {
        return SelfRepairOutcome::Skipped;
    };
    let dispatch_message = format!(
        "{}\n\n{}.\n\n{}\n\njob: {}",
        trigger.marker(),
        trigger.what(),
        trigger.instruction(),
        info.id,
    );
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item.id.clone()),
        body: Some(dispatch_message.clone()),
        ..Default::default()
    });
    update_pr_stage(
        folder_path,
        pr_number,
        stale_stage_label(labels),
        SELF_REPAIR_PR_LABEL,
        &dispatch_message,
    );
    SelfRepairOutcome::Dispatched
}

/// CodeRabbit review comments a live GraphQL call reports as still
/// unresolved (`resolved_ids` -- see `pulls::resolved_review_comment_ids`'s
/// doc comment), filtered to the ones CodeRabbit itself left rather than a
/// human reviewer's -- `run_review_sweep` only auto-dispatches a repair job
/// for the former; a human's own unresolved review comment is left for the
/// approval-gate flow instead. Split out from `coderabbit_repair_or_gate` as
/// a pure function so the filter itself is unit-testable without a live
/// GitHub client.
fn unresolved_coderabbit_comments<'a>(
    review_comments: &'a [crate::github::models::ReviewComment],
    resolved_ids: &std::collections::HashSet<u64>,
) -> Vec<&'a crate::github::models::ReviewComment> {
    review_comments
        .iter()
        .filter(|c| {
            !resolved_ids.contains(&c.id)
                && c.user
                    .login
                    .to_lowercase()
                    .starts_with(CODERABBIT_LOGIN_PREFIX)
        })
        .collect()
}

/// Live-fetches `pr_number`'s unresolved CodeRabbit findings -- split out
/// from `coderabbit_repair_or_gate` so that function's own cap/claim/dispatch
/// decision tree takes pre-fetched findings as a plain slice, exactly like
/// `self_repair_or_gate` takes a pre-fetched `failed_checks`, and so it can
/// be unit-tested the same soft-fail-tolerant way (no live GitHub client)
/// `self_repair_or_gate`'s own tests already rely on. Soft-fails to an empty
/// list on any lookup error, same as every other GitHub-touching helper in
/// this file -- the caller's fallback is simply to skip this tick and try
/// again next time.
fn fetch_unresolved_coderabbit_comments(
    repo_root: &std::path::Path,
    pr_number: u64,
) -> Vec<crate::github::models::ReviewComment> {
    let Some(repo) = crate::github::RepoId::resolve_from_remote(repo_root) else {
        return Vec::new();
    };
    let Ok(client) = crate::github::Client::new() else {
        return Vec::new();
    };
    let Ok(review_comments) =
        crate::github::pulls::list_review_comments(&client, &repo, pr_number, None)
    else {
        return Vec::new();
    };
    let Ok(resolved_ids) =
        crate::github::pulls::resolved_review_comment_ids(&client, &repo, pr_number)
    else {
        return Vec::new();
    };
    unresolved_coderabbit_comments(&review_comments, &resolved_ids)
        .into_iter()
        .cloned()
        .collect()
}

/// Dispatches a review-repair job for a CI-green PR whose CodeRabbit review
/// still has unresolved findings, or -- once
/// `quota::decide::SELF_REPAIR_CAP` prior attempts have been made -- gates
/// it for a human instead of retrying forever. Item #273: `run_review_sweep`
/// previously only ever reacted to CI check status and `mergeable_state`; a
/// PR could sit with CodeRabbit review threads flagged and untouched
/// indefinitely as long as CI itself stayed green. Mirrors
/// `self_repair_or_gate` throughout -- same cap accounting via a marker
/// comment prefix, same claim/cooldown/host-pressure gates before
/// dispatching, same PR-stage-label convention -- so the two dispatch paths
/// can't quietly drift apart. `findings` is fetched once by the caller
/// (`fetch_unresolved_coderabbit_comments`) rather than by this function
/// itself, again mirroring how `self_repair_or_gate` receives `failed_checks`.
#[allow(clippy::too_many_arguments)]
fn coderabbit_repair_or_gate(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    auth_conn: &rusqlite::Connection,
    host_policy: agentflare_resource_gate::Policy,
    item: &agentflare_backend::item::Item,
    pr_number: u64,
    findings: &[crate::github::models::ReviewComment],
    labels: &[String],
    label_id_by_name: &std::collections::HashMap<String, String>,
    folder_path: &str,
) -> SelfRepairOutcome {
    if already_gated_or_in_flight(mcp, queue, item, label_id_by_name) {
        return SelfRepairOutcome::Skipped;
    }

    if findings.is_empty() {
        // Findings from an earlier tick all got resolved since -- swap the
        // stage label back rather than leaving a stale "review-repair" label
        // on a PR nothing is actively repairing anymore. Only touches GitHub
        // when the label is actually still there.
        if labels.iter().any(|l| l == CODERABBIT_REPAIR_PR_LABEL) {
            update_pr_stage(
                folder_path,
                pr_number,
                Some(CODERABBIT_REPAIR_PR_LABEL),
                IN_REVIEW_PR_LABEL,
                "## supervisor — CodeRabbit review clear\n\nNo unresolved findings remain.",
            );
        }
        return SelfRepairOutcome::Skipped;
    }

    let prior_attempts = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item.id))
        .ok()
        .and_then(Result::ok)
        .map(|comments| {
            comments
                .iter()
                .filter(|c| c.body.starts_with(CODERABBIT_REPAIR_MARKER))
                .count() as u32
        })
        .unwrap_or(0);

    if prior_attempts >= crate::quota::decide::SELF_REPAIR_CAP {
        let cap_message = format!(
            "## supervisor — CodeRabbit review repair cap reached\n\n{} unresolved finding(s) \
             remain. {} automatic repair attempt(s) already made — needs a human look.",
            findings.len(),
            crate::quota::decide::SELF_REPAIR_CAP,
        );
        let _ = mcp.comment_impl(CommentRequest {
            action: "create".into(),
            item_id: Some(item.id.clone()),
            body: Some(cap_message.clone()),
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
        update_pr_stage(
            folder_path,
            pr_number,
            Some(CODERABBIT_REPAIR_PR_LABEL),
            NEEDS_HUMAN_PR_LABEL,
            &cap_message,
        );
        notify_human_gate(
            item,
            &format!(
                "CodeRabbit review repair cap reached ({} attempt(s), {} finding(s) still \
                 unresolved)",
                crate::quota::decide::SELF_REPAIR_CAP,
                findings.len()
            ),
        );
        return SelfRepairOutcome::Skipped;
    }

    // Same item #114 rationale as `self_repair_or_gate`: dispatching while
    // the item's own claim is still live would just die instantly at
    // `execute_work`'s claim-acquire step.
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

    // Same item #261 rationale as `self_repair_or_gate`: arbitrate across
    // workstations via a claim marker comment on the PR itself, since a
    // per-workstation backend db can't see another workstation's dispatch.
    // Same soft-fail as `self_repair_or_gate`'s own resolution here: no
    // remote/no credentials proceeds rather than blocking dispatch on it.
    let repo_and_client =
        crate::github::RepoId::resolve_from_remote(std::path::Path::new(folder_path))
            .zip(crate::github::Client::new().ok());
    if let Some((repo, client)) = repo_and_client
        && !claim_self_repair(&client, &repo, pr_number, &item.id, crate::claims::now())
    {
        return SelfRepairOutcome::Deferred;
    }

    let Some(agent) = item
        .assignee_agent
        .as_deref()
        .and_then(resolve_confirmed_agent)
        .or_else(|| route_unassigned(item))
    else {
        return SelfRepairOutcome::Skipped;
    };
    if crate::auth_db::is_cooling_down(auth_conn, agent.as_str()) {
        return SelfRepairOutcome::Deferred;
    }
    if host_policy.blocks_dispatch() {
        return SelfRepairOutcome::Deferred;
    }

    // Summarized, not dumped in full -- a long CodeRabbit finding body would
    // otherwise blow up the dispatch comment for a PR with many of them.
    let summary: Vec<String> = findings
        .iter()
        .take(10)
        .map(|c| {
            let line = c.line.map(|l| format!(":{l}")).unwrap_or_default();
            let first_line = c.body.lines().next().unwrap_or("").trim();
            format!("- `{}{line}` ({}): {first_line}", c.path, c.user.login)
        })
        .collect();
    let overflow = findings.len().saturating_sub(10);
    let overflow_line = if overflow > 0 {
        format!("\n- …and {overflow} more")
    } else {
        String::new()
    };

    let reason = format!(
        "CodeRabbit review: {} unresolved finding(s)",
        findings.len()
    );
    let Some(info) = enqueue_work_job(queue, item, agent, Some(folder_path), Some(&reason)) else {
        return SelfRepairOutcome::Skipped;
    };
    let dispatch_message = format!(
        "{CODERABBIT_REPAIR_MARKER}\n\nCodeRabbit left {} unresolved finding(s) on this PR:\n\n\
         {}{overflow_line}\n\nPlease address them and push a fix.\n\njob: {}",
        findings.len(),
        summary.join("\n"),
        info.id,
    );
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item.id.clone()),
        body: Some(dispatch_message.clone()),
        ..Default::default()
    });
    update_pr_stage(
        folder_path,
        pr_number,
        Some(IN_REVIEW_PR_LABEL),
        CODERABBIT_REPAIR_PR_LABEL,
        &dispatch_message,
    );
    SelfRepairOutcome::Dispatched
}

/// TTL a self-repair PR claim marker stays live for. Reuses the same
/// duration as the backend item-claim TTL (`claim_still_live` above) rather
/// than a bespoke constant, since both bound how long a single self-repair
/// attempt is expected to run before it's fair game for another workstation
/// to retry.
fn self_repair_claim_ttl_secs() -> i64 {
    crate::mcp_server::types::backend_claim_ttl_secs()
}

/// Cross-machine arbitration for self-repair dispatch (item #261). Reuses
/// `github::bridge::claim`'s TTL+heartbeat marker resolution -- the same
/// optimistic two-step `github::bridge::tick::try_claim` already uses for
/// issue claiming (post our marker, re-read, confirm we're still the
/// earliest live claimant) -- against a comment on the PR itself, since a PR
/// is also an issue on GitHub's comments endpoint. Returns `false` only when
/// another workstation is DEFINITELY already holding a live claim; any
/// GitHub error along the way soft-fails toward `true` (proceed) rather than
/// blocking every repair attempt on a transient API hiccup -- a rare missed
/// race producing one duplicate self-repair dispatch is a far smaller harm
/// than self-repair silently never running again, the same trade-off
/// `discover_untracked_prs`'s own `find_existing` soft-fail already makes.
fn claim_self_repair(
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    pr_number: u64,
    item_id: &str,
    now: i64,
) -> bool {
    use crate::github::bridge::claim as claim_rules;
    use crate::github::bridge::marker::{Action, Marker};

    let ttl = self_repair_claim_ttl_secs();
    let me = crate::github::bridge::config::machine_label();

    let Ok(before) = crate::github::issues::list_comments(client, repo, pr_number, None) else {
        return true;
    };
    let before: Vec<(u64, String)> = before.into_iter().map(|c| (c.id, c.body)).collect();
    match claim_rules::resolve_holder(&before, now, ttl) {
        Some(h) if h.marker.owner == me => return true,
        Some(_) => return false,
        None => {}
    }

    let marker = Marker {
        action: Action::Claim,
        owner: me.clone(),
        item: item_id.to_string(),
        ts: now,
        hash: String::new(),
    };
    if crate::github::issues::comment(
        client,
        repo,
        pr_number,
        &format!("Claiming self-repair for `{me}`.\n\n{}", marker.render()),
    )
    .is_err()
    {
        return true;
    }

    let Ok(after) = crate::github::issues::list_comments(client, repo, pr_number, None) else {
        return true;
    };
    let after: Vec<(u64, String)> = after.into_iter().map(|c| (c.id, c.body)).collect();
    claim_rules::i_hold(&after, &me, now, ttl)
}

#[cfg(test)]
#[path = "supervisor_tests.rs"]
mod tests;
