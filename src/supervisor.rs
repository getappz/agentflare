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
/// Set by an operator pause (`item(action="pause")`, `agentflare item
/// pause`): the item's run is parked with its worktree and run state kept,
/// and its claim released. Discovery never dispatches an item carrying it,
/// even if `ready-for-work` is added back by hand -- only a resume (which
/// removes it) re-arms the item.
pub(crate) const PAUSED_LABEL: &str = "paused";
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
        eprintln!(
            "agentflare-supervisor: could not add {to} to PR #{number}: {}",
            e.log_safe()
        );
    }
    if let Some(from) = from
        && let Err(e) = crate::github::issues::remove_label(&client, &repo, number, from)
    {
        eprintln!(
            "agentflare-supervisor: could not remove {from} from PR #{number}: {}",
            e.log_safe()
        );
    }
    // Empty comment = label bookkeeping only (a silent re-dispatch whose
    // announcement already went out) — never post a blank comment to the PR.
    if !comment.is_empty()
        && let Err(e) = crate::github::issues::comment(&client, &repo, number, comment)
    {
        eprintln!(
            "agentflare-supervisor: could not comment on PR #{number}: {}",
            e.log_safe()
        );
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

/// Most work jobs one project may have queued or running at once while
/// other projects also have ready-for-work items waiting, so one project's
/// backlog can't take every worker. `AGENTFLARE_WORK_MAX_PER_PROJECT`
/// overrides it; otherwise half the worker pool (the same
/// `AGENTFLARE_WORK_MAX_CONCURRENCY` / resource-gate sizing the daemon's
/// `WorkerPool` starts with), never less than one.
fn per_project_work_cap() -> u64 {
    let parse = |var: &str| {
        std::env::var(var)
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|n| *n > 0)
    };
    if let Some(cap) = parse("AGENTFLARE_WORK_MAX_PER_PROJECT") {
        return cap;
    }
    let workers = parse("AGENTFLARE_WORK_MAX_CONCURRENCY").unwrap_or_else(|| {
        let available_parallelism = std::thread::available_parallelism().map_or(1, |n| n.get());
        agentflare_resource_gate::pool_size::resolve_pool_size(
            available_parallelism,
            agentflare_resource_gate::pool_size::memory_budget_bytes(),
        ) as u64
    });
    (workers / 2).max(1)
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
            // One project's broken or vanished folder (repo deleted/moved,
            // unmounted drive) must not stall dispatch for every other
            // project -- skip just this one, loudly, and look again next tick.
            if !std::path::Path::new(&dir.folder_path).is_dir() {
                eprintln!(
                    "agentflare-supervisor: project {} is registered at {} but that folder does \
                     not exist -- skipping its ready-for-work items this tick",
                    dir.project_id, dir.folder_path
                );
                continue;
            }
            // Same isolation for a per-project DB read failure: `?` here
            // used to abort the whole tick, for every project.
            let labels = match agentflare_backend::label::list_by_project(conn, &dir.project_id) {
                Ok(labels) => labels,
                Err(e) => {
                    eprintln!(
                        "agentflare-supervisor: could not list labels for project {}: {e}",
                        dir.project_id
                    );
                    continue;
                }
            };
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
                match agentflare_backend::item::list_by_label(conn, &dir.project_id, &ready_id) {
                    Ok(items) => items,
                    Err(e) => {
                        eprintln!(
                            "agentflare-supervisor: could not list ready-for-work items for \
                             project {}: {e}",
                            dir.project_id
                        );
                        continue;
                    }
                };
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

    // Per-project fairness: only once more than one project is competing
    // for workers this tick -- a lone project may use the whole pool.
    let contended = batches.iter().filter(|b| !b.items.is_empty()).count() > 1;
    let project_cap = per_project_work_cap();

    for batch in batches {
        let ProjectBatch {
            folder_path,
            items,
            label_id_by_name,
            ready_id,
        } = batch;
        let mut project_in_flight = if contended {
            queue
                .count_active_with_arg(&folder_path, Some(2))
                .unwrap_or(0)
        } else {
            0
        };
        for item in items {
            if let Some(paused_id) = label_id_by_name.get(PAUSED_LABEL) {
                let paused = mcp
                    .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item.id))
                    .ok()
                    .and_then(Result::ok)
                    .is_some_and(|ids| ids.contains(paused_id));
                if paused {
                    result.waiting += 1;
                    continue;
                }
            }
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
                    if contended && project_in_flight >= project_cap {
                        // Leave ready-for-work in place, same as the other
                        // Wait paths: the next tick re-checks the cap.
                        eprintln!(
                            "agentflare-supervisor: item #{} ({}) is ready-for-work but its \
                             project already has {project_in_flight} work job(s) queued or \
                             running (per-project cap {project_cap})",
                            item.sequence_id, item.id
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
                        DispatchOutcome::Dispatched => {
                            result.dispatched += 1;
                            project_in_flight += 1;
                        }
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
            let reason = "auto-gated: needs a plan — call item(action=\"submit_plan\", \
                           plan_asset_id=...) before this item can be dispatched";
            notify_human_gate(item, reason);
            // `notify_human_gate` above is a best-effort, opt-in Telegram ping
            // (no-ops entirely without a configured chat id) -- unlike
            // `skip_item`'s "no assignee_agent" case, nothing wrote *why* this
            // item is stuck into its own comment thread, so a human who isn't
            // watching Telegram (or wasn't configured at all) sees only a
            // silent item and the daemon's own stderr. Deliberately does NOT
            // touch labels the way `record_supervisor_action` does -- this
            // item keeps `ready-for-work` by design (see `WaitingOnPlan`'s
            // doc comment) so the very next tick re-checks it once approved.
            let outcome = mcp.with_backend_db(|conn| {
                agentflare_backend::comment::create(
                    conn,
                    &item.id,
                    &crate::claims::owner_id(),
                    &format!("## supervisor — waiting on plan\n\n{reason}"),
                )
            });
            let comment_err = match outcome {
                Ok(Ok(_)) => None,
                Ok(Err(e)) => Some(e.to_string()),
                Err(e) => Some(e.to_string()),
            };
            if let Some(e) = comment_err {
                eprintln!(
                    "agentflare-supervisor: failed to record plan-gate comment on item #{} ({}): {e}",
                    item.sequence_id, item.id
                );
            }
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

/// Marker prefix on the one-time repair-complete summary for each trigger --
/// counterparts to `CI_SELF_REPAIR_MARKER`/`CONFLICT_REPAIR_MARKER`'s dispatch
/// announcements, mirroring `CODERABBIT_REPAIR_COMPLETE_MARKER` below.
const CI_SELF_REPAIR_COMPLETE_MARKER: &str = "## supervisor — CI self-repair complete";
const CONFLICT_REPAIR_COMPLETE_MARKER: &str = "## supervisor — merge-conflict repair complete";

/// Item-metadata keys tracking CI-self-repair announcements (item #303: same
/// class of bug item #633 fixed for `coderabbit_repair_or_gate` -- see
/// `CODERABBIT_REPAIR_ANNOUNCED_KEY` below for the shared rationale). Kept
/// under their own key namespace, not reused from the CodeRabbit set, so the
/// two repair paths' caps/fingerprints can never bleed into each other.
const CI_SELF_REPAIR_ANNOUNCED_KEY: &str = "ci_self_repair_announced_for";
const CI_SELF_REPAIR_SILENT_KEY: &str = "ci_self_repair_silent_attempts";
const CI_SELF_REPAIR_COMPLETED_KEY: &str = "ci_self_repair_completed_for";
/// Same convention, for the merge-conflict-repair trigger -- kept separate
/// from the CI-self-repair keys above for the same reason
/// `CONFLICT_REPAIR_MARKER` is kept separate from `CI_SELF_REPAIR_MARKER`:
/// burning one trigger's cap must not block the other's.
const CONFLICT_REPAIR_ANNOUNCED_KEY: &str = "conflict_repair_announced_for";
const CONFLICT_REPAIR_SILENT_KEY: &str = "conflict_repair_silent_attempts";
const CONFLICT_REPAIR_COMPLETED_KEY: &str = "conflict_repair_completed_for";

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

    fn announced_key(&self) -> &'static str {
        match self {
            Self::FailingChecks(_) => CI_SELF_REPAIR_ANNOUNCED_KEY,
            Self::MergeConflict => CONFLICT_REPAIR_ANNOUNCED_KEY,
        }
    }

    fn silent_key(&self) -> &'static str {
        match self {
            Self::FailingChecks(_) => CI_SELF_REPAIR_SILENT_KEY,
            Self::MergeConflict => CONFLICT_REPAIR_SILENT_KEY,
        }
    }

    fn completed_key(&self) -> &'static str {
        match self {
            Self::FailingChecks(_) => CI_SELF_REPAIR_COMPLETED_KEY,
            Self::MergeConflict => CONFLICT_REPAIR_COMPLETED_KEY,
        }
    }

    /// Stable fingerprint of "what's wrong" right now -- sorted failing-check
    /// names for `FailingChecks` (so a re-fetch in a different order doesn't
    /// re-announce, but a genuinely different failing set does), a constant
    /// for `MergeConflict` since there's only ever one such state. Mirrors
    /// `coderabbit_findings_fingerprint`'s role for the CodeRabbit path.
    fn fingerprint(&self) -> String {
        match self {
            Self::FailingChecks(checks) => {
                let mut sorted = checks.to_vec();
                sorted.sort();
                sorted.join(",")
            }
            Self::MergeConflict => "merge-conflict".to_string(),
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

/// Reads a repair-tracking triple off an item's metadata: the announced
/// fingerprint, the silent re-dispatch count, and the completed fingerprint.
/// Missing/corrupt metadata reads as all-empty, never an error. Key names are
/// parameters (not hardcoded) so `self_repair_or_gate`'s CI-self-repair and
/// merge-conflict-repair triggers and `coderabbit_repair_or_gate` can each
/// track their own state under their own metadata keys without duplicating
/// this function -- see `RepairTrigger::announced_key`/`silent_key`/
/// `completed_key` and the `CODERABBIT_REPAIR_*_KEY` constants for the actual
/// key sets.
fn repair_track(
    item: &agentflare_backend::item::Item,
    announced_key: &str,
    silent_key: &str,
    completed_key: &str,
) -> (Option<String>, u32, Option<String>) {
    let meta: serde_json::Value = serde_json::from_str(&item.metadata).unwrap_or_default();
    let text = |k: &str| meta.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let count = meta
        .get(silent_key)
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0) as u32;
    (text(announced_key), count, text(completed_key))
}

/// Merges a repair-tracking triple into the item's current metadata
/// (re-fetched, not reused from any snapshot, so a concurrent metadata write
/// isn't clobbered — same pattern as `persist_comment_cursor`). Each value is
/// `Option`/`bool`: `None`/`false` leaves that key untouched. Key names are
/// parameters, mirroring `repair_track` above.
#[allow(clippy::too_many_arguments)]
fn persist_repair_track(
    mcp: &AgentflareMcp,
    item_id: &str,
    announced_key: &str,
    silent_key: &str,
    completed_key: &str,
    announced: Option<&str>,
    silent_bump: bool,
    completed: Option<&str>,
) {
    let Ok(raw) = mcp.item_get(ItemRequest {
        action: "get".into(),
        id: Some(item_id.to_string()),
        ..Default::default()
    }) else {
        return;
    };
    let Ok(item) = serde_json::from_str::<agentflare_backend::item::Item>(&raw) else {
        return;
    };
    let mut merged = serde_json::from_str::<serde_json::Value>(&item.metadata)
        .ok()
        .and_then(|v| v.as_object().cloned())
        .map(serde_json::Value::Object)
        .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
    if let Some(fp) = announced {
        merged[announced_key] = serde_json::Value::String(fp.to_string());
    }
    if silent_bump {
        let current = merged
            .get(silent_key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        merged[silent_key] = serde_json::Value::from(current + 1);
    }
    if let Some(fp) = completed {
        merged[completed_key] = serde_json::Value::String(fp.to_string());
    }
    let _ = mcp.item_update(ItemRequest {
        action: "update".into(),
        id: Some(item_id.to_string()),
        metadata: Some(merged),
        ..Default::default()
    });
}

/// Posts the one-time repair summary once the announced trigger has cleared
/// (no more findings / CI green / conflict resolved, depending on the caller),
/// and returns an excerpt for the PR-side clear message when the repair run
/// left a recorded outcome. Returns `None` when there is nothing to announce
/// (never dispatched) or the summary already went out — callers post nothing
/// more in either case. `dispatch_marker`/`complete_marker`/`resolved_text`
/// and the metadata key triple are parameters so this one function serves
/// `coderabbit_repair_or_gate` and both `self_repair_or_gate` triggers
/// without duplicating it -- mirrors `repair_track`/`persist_repair_track`.
#[allow(clippy::too_many_arguments)]
fn maybe_post_repair_complete_summary(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    dispatch_marker: &str,
    complete_marker: &str,
    resolved_text: &str,
    announced_key: &str,
    silent_key: &str,
    completed_key: &str,
) -> Option<String> {
    let (announced_for, _, completed_for) =
        repair_track(item, announced_key, silent_key, completed_key);
    let announced = announced_for?;
    if completed_for.as_deref() == Some(announced.as_str()) {
        return None;
    }
    // Latest repair-run outcome after the last dispatch marker, if any.
    let excerpt = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item.id))
        .ok()
        .and_then(Result::ok)
        .and_then(|comments| {
            let last_dispatch = comments
                .iter()
                .rposition(|c| c.body.starts_with(dispatch_marker))?;
            comments[last_dispatch..].iter().rev().find_map(|c| {
                c.body
                    .strip_prefix(crate::dispatch_failure_ceiling::WORK_SUCCESS_MARKER)
                    .map(|rest| {
                        rest.trim()
                            .lines()
                            .take(20)
                            .collect::<Vec<_>>()
                            .join("\n")
                            .chars()
                            .take(1200)
                            .collect::<String>()
                    })
            })
        })
        .filter(|text| !text.trim().is_empty());
    let body = match &excerpt {
        Some(text) => {
            format!("{complete_marker}\n\n{resolved_text}. What the repair run reported:\n\n{text}")
        }
        None => format!(
            "{complete_marker}\n\n{resolved_text} \
             (no repair-run summary on record — likely fixed by a manual push)."
        ),
    };
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item.id.clone()),
        body: Some(body),
        ..Default::default()
    });
    persist_repair_track(
        mcp,
        &item.id,
        announced_key,
        silent_key,
        completed_key,
        None,
        false,
        Some(&announced),
    );
    excerpt
}

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
    /// Items whose PR was closed without merging, sent back to the backlog
    /// for a fresh attempt instead of sitting in "in_review" forever.
    pub requeued: usize,
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

/// What `merge_or_repair_findings` decided for a CI-green PR. Distinct from
/// `SelfRepairOutcome` because "merged" isn't a repair outcome at all --
/// keeping them separate means the merge branch and the repair branch can't
/// be conflated at the call site.
enum PassingPrOutcome {
    Merged,
    NotMerged,
    Repair(SelfRepairOutcome),
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
        requeued: 0,
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
        // Every per-item call below (`item_check_merge`, `comment_impl`,
        // `item_add_label`, ...) resolves ids through `resolve_item_id`,
        // which only accepts items of the instance's own project. The
        // daemon's `mcp` is linked to whatever repo it was started in, so
        // with it an in_review item from any other registered project could
        // never be promoted, relabeled or cleaned up and sat in_review
        // forever. Pin a view of it to this batch's project and folder.
        let scoped = mcp.scoped_to_project(project_id.clone(), repo_root.clone());
        let mcp = &scoped;
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
            if promote_merged_item(mcp, item, repo_root) {
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
        crate::worktree::PrCiStatus::Passing {
            number,
            labels,
            head_sha,
        } => handle_ci_green(
            mcp,
            queue,
            auth_conn,
            host_policy,
            item,
            number,
            &labels,
            CiGreenMerge::Allowed {
                head_sha: head_sha.as_deref(),
            },
            label_id_by_name,
            folder_path,
            repo_root,
            result,
        ),
        // Branch protection is holding a CI-green PR for a human review:
        // everything `Passing` does short of the merge attempt -- stale
        // repair labels cleared, the approval gate surfaced (the whole point:
        // this used to read as `Pending` and never reached the gate), and
        // CodeRabbit findings still repaired while it waits.
        crate::worktree::PrCiStatus::AwaitingReview { number, labels } => handle_ci_green(
            mcp,
            queue,
            auth_conn,
            host_policy,
            item,
            number,
            &labels,
            CiGreenMerge::BlockedOnReview,
            label_id_by_name,
            folder_path,
            repo_root,
            result,
        ),
        crate::worktree::PrCiStatus::Behind { number, head_sha } => {
            if crate::worktree::update_stale_branch(repo_root, number, head_sha.as_deref()) {
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
        crate::worktree::PrCiStatus::Closed { number } => {
            if requeue_closed_pr_item(mcp, item, number) {
                result.requeued += 1;
            } else {
                result.skipped += 1;
            }
        }
        // A review bot's own pending "review paused" status is one way a PR
        // reads as pending forever: the bot never reviews the new head on
        // its own, and nothing else here would summon it.
        crate::worktree::PrCiStatus::Pending { number, head_sha } => {
            if let Some(head) = head_sha.as_deref() {
                nudge_paused_review_for_pending(mcp, item, repo_root, number, head);
            }
            result.skipped += 1;
        }
        crate::worktree::PrCiStatus::Unknown => {
            result.skipped += 1;
        }
    }
}

/// Called from `item_check_merge` right after `item_id` is promoted to
/// `completed` (both the automatic path via `merge::promote_merged_item` and
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
///
/// A targeted count over every active row, not `Queue::list` -- that only
/// returns the 100 newest jobs, so an older still-queued job for this item
/// was invisible to this guard once enough other work piled up behind it.
fn job_in_flight(queue: &agentflare_jobs::Queue, item_id: &str) -> bool {
    queue
        .count_active_with_arg(item_id, None)
        .is_ok_and(|n| n > 0)
}

/// Telegram notifications and the inbound channel-approval poll. Split out
/// when item #573's plan-gate work pushed this file past the LOC gate; glob
/// re-exported so every existing `crate::supervisor::notify_*` /
/// `first_time_gated` path (and `supervisor_tests.rs`'s `use super::*`)
/// keeps working unchanged.
pub(crate) mod notify;
pub(crate) use notify::*;
mod merge;
use merge::*;
mod review_bots;
pub(crate) mod review_findings;
use review_bots::*;

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

    // Item #303: marker-comment counting alone stayed at 0 across hundreds of
    // real dispatches (`list_by_item` returning an empty/stale result, live
    // on items #300/#54), so the cap never tripped. A metadata-persisted
    // silent-attempt counter (same fix as item #633's `coderabbit_repair_or_gate`)
    // backstops it: `prior_attempts` is the marker-comment count PLUS this
    // counter, so a retry that intentionally skips the marker comment (the
    // fingerprint-unchanged branch below) still counts toward the cap.
    let (announced_for, silent_attempts, _) = repair_track(
        item,
        trigger.announced_key(),
        trigger.silent_key(),
        trigger.completed_key(),
    );
    let fingerprint = trigger.fingerprint();
    let prior_markers = mcp
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
    let prior_attempts = prior_markers + silent_attempts;

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
    if announced_for.as_deref() == Some(fingerprint.as_str()) {
        // Same "what's wrong" already announced — re-dispatch the retry
        // silently instead of posting the identical announcement again
        // (mirrors `coderabbit_repair_or_gate`'s own fingerprint-unchanged
        // branch, item #633/#303).
        persist_repair_track(
            mcp,
            &item.id,
            trigger.announced_key(),
            trigger.silent_key(),
            trigger.completed_key(),
            None,
            true,
            None,
        );
        // The PR stage label may have been reverted out-of-band; ensure it
        // without commenting (empty comment = label bookkeeping only).
        if !labels.iter().any(|l| l == SELF_REPAIR_PR_LABEL) {
            update_pr_stage(
                folder_path,
                pr_number,
                stale_stage_label(labels),
                SELF_REPAIR_PR_LABEL,
                "",
            );
        }
        return SelfRepairOutcome::Dispatched;
    }
    persist_repair_track(
        mcp,
        &item.id,
        trigger.announced_key(),
        trigger.silent_key(),
        trigger.completed_key(),
        Some(&fingerprint),
        false,
        None,
    );
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

#[cfg(test)]
#[path = "supervisor_coderabbit_tests.rs"]
mod coderabbit_tests;

#[cfg(test)]
#[path = "supervisor/tests/review_bots_tests.rs"]
mod review_bots_tests;

#[cfg(test)]
#[path = "supervisor_self_repair_tests.rs"]
mod self_repair_tests;
