//! Split out of `dashboard::server` to keep that file under the LOC gate
//! (`scripts/loc-gate.sh`) -- this is purely a home for
//! `reconcile_orphaned_jobs` and its helper, not a new subsystem boundary.

use agentflare_jobs::Queue;

/// Sweeps `agent_jobs` for rows left `state = 'running'` by a previous
/// daemon process's death (`Queue::reconcile_orphaned_running`'s doc
/// comment has the full root-cause trace — item #40) and releases whatever
/// work-item claim each one held, exactly the way `execute_work`'s own
/// failure path already does (`cli::work::release_and_comment`) rather than
/// inventing a second release mechanism. Must run before `worker_pool.start`
/// — see `reconcile_orphaned_running`'s doc comment for why call order
/// matters here.
pub(super) fn reconcile_orphaned_jobs(queue: &Queue) {
    let orphaned = match queue.reconcile_orphaned_running() {
        Ok(jobs) => jobs,
        Err(e) => {
            eprintln!("agentflare-jobs: orphan reconciliation failed: {e}");
            return;
        }
    };
    if orphaned.is_empty() {
        return;
    }
    eprintln!(
        "agentflare-jobs: reconciled {} job(s) left running by a previous daemon process",
        orphaned.len()
    );
    for (job_id, job) in orphaned {
        // Only `WorkItemExecutor`-dispatched jobs (see `enqueue_work_job` in
        // `supervisor.rs`) hold an item claim at all -- a plain subprocess
        // job submitted via `POST /api/jobs` has no item to release.
        if !job.in_process {
            continue;
        }
        let (Some(item_id), Some(agent)) = (job.args.first(), job.args.get(1)) else {
            continue;
        };
        // Mirrors `WorkItemExecutor::execute`'s own project scoping and
        // owner-id convention exactly, so `release_and_comment` resolves the
        // same item/claim the dead job itself would have.
        let mcp = match job.args.get(2) {
            Some(folder_path) => crate::mcp_server::AgentflareMcp::for_project_dir(
                std::path::PathBuf::from(folder_path),
            ),
            None => crate::mcp_server::AgentflareMcp::default(),
        };
        // Kill any process still touching the worktree BEFORE releasing
        // below: `run_headless`'s subprocess (own process group, needed for
        // `kill_tree`) survives a daemon restart as an orphan, invisible to
        // the DB-only reconciliation above (item #164).
        if let Some(folder_path) = job.args.get(2) {
            let repo_root = std::path::PathBuf::from(folder_path);
            let worktree_path = mcp
                .with_backend_db(|conn| {
                    agentflare_backend::item::get(conn, item_id)
                        .ok()
                        .map(|item| item.sequence_id)
                })
                .ok()
                .flatten()
                .map(|seq| {
                    repo_root
                        .join(".worktrees")
                        .join("task")
                        .join(seq.to_string())
                });
            if let Some(path) = worktree_path {
                kill_processes_touching_worktree(&path);
            }
        }
        let owner = format!("{agent}:{job_id}");
        crate::claims::with_owner_override(owner, || {
            crate::cli::work::release_and_comment(
                &mcp,
                item_id,
                "orphaned by daemon restart",
                None,
            );
        });
        if restore_ready_for_work(&mcp, item_id, agent, &job_id) {
            post_any_reason_cap_comment(&mcp, item_id);
        }
    }
}

/// Force-kills every process whose command line references `worktree_path`
/// (see the call site for why) -- matched by substring on the full command
/// line, not a tracked PID, so it catches the whole process tree
/// (bwrap layers, grandchildren). Best-effort: a failing lookup is swallowed.
///
/// The command-line match only works where the path is in argv (the Linux
/// bwrap wrapper); an unsandboxed agent CLI has it only as its cwd, with the
/// prompt on stdin. So this also kills the process groups `agent_launch`
/// recorded for the worktree at spawn time, and on Linux any process whose
/// cwd is inside it.
#[cfg(unix)]
fn kill_processes_touching_worktree(worktree_path: &std::path::Path) {
    kill_recorded_agent_processes(worktree_path);
    #[cfg(target_os = "linux")]
    kill_processes_with_cwd_under(worktree_path);
    let pattern = worktree_path.to_string_lossy().into_owned();
    let Ok(output) = std::process::Command::new("pgrep")
        .args(["-f", &pattern])
        .output()
    else {
        return;
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Ok(pid) = line.trim().parse::<u32>() {
            let _ = crate::ipc::process::force_kill(pid);
        }
    }
}

/// Windows has no `pgrep`; `Get-CimInstance Win32_Process` exposes each
/// process's `CommandLine` for the same match. `''`-escapes a stray quote.
/// Recorded agent pids are killed first, same as the Unix variant.
#[cfg(windows)]
fn kill_processes_touching_worktree(worktree_path: &std::path::Path) {
    kill_recorded_agent_processes(worktree_path);
    let pattern = worktree_path
        .to_string_lossy()
        .into_owned()
        .replace('\'', "''");
    let script = format!(
        "Get-CimInstance Win32_Process | Where-Object {{ $_.CommandLine -like '*{pattern}*' }} \
         | ForEach-Object {{ $_.ProcessId }}"
    );
    // Background reconcile tick runs console-less; hidden spawn or Windows flashes.
    let Ok(output) = flare_process::command("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
    else {
        return;
    };
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Ok(pid) = line.trim().parse::<u32>() {
            let _ = crate::ipc::process::force_kill(pid);
        }
    }
}

/// Kills (and forgets) every agent CLI `agent_launch::run_captured_for_job`
/// recorded as launched in `worktree_path` -- see `agent_launch::agent_pid_dir`.
/// A record whose pid is alive but now belongs to a different process (its
/// start time no longer matches, or on Unix it's no longer the leader of its
/// own process group) is dropped without killing anything.
fn kill_recorded_agent_processes(worktree_path: &std::path::Path) {
    for record in crate::agent_launch::recorded_agent_pids(worktree_path) {
        let pid = record.pid;
        if pid != std::process::id() {
            let alive = crate::ipc::process::is_alive(pid);
            let reused = alive
                && match (
                    record.start_token.as_deref(),
                    crate::agent_launch::process_start_token(pid).as_deref(),
                ) {
                    (Some(recorded), Some(current)) => recorded != current,
                    _ => !is_own_group_leader(pid),
                };
            if !reused {
                kill_recorded_agent_group(pid, alive);
            }
        }
        let _ = std::fs::remove_file(&record.record_path);
    }
}

/// Whether `pid` still leads the process group it was launched as the
/// leader of (`process_group(0)`) -- a cheap plausibility check for a reused
/// pid when no start time is available.
#[cfg(unix)]
fn is_own_group_leader(pid: u32) -> bool {
    #[allow(unsafe_code)]
    // SAFETY: `getpgid` has no memory-safety preconditions.
    let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
    pgid == pid as libc::pid_t
}

#[cfg(windows)]
fn is_own_group_leader(_pid: u32) -> bool {
    // No process groups; without a start time there's nothing to confirm
    // the pid is still ours, so don't kill it.
    false
}

/// Kills the whole process group the agent was launched as the leader of.
/// Signalled even when the leader itself is gone: its descendants keep the
/// group alive, and while any member exists the OS won't hand that id to a
/// new process, so the group can only still be ours.
#[cfg(unix)]
fn kill_recorded_agent_group(pgid: u32, _leader_alive: bool) {
    #[allow(unsafe_code)]
    // SAFETY: `kill` has no memory-safety preconditions; a negative pid
    // targets the process group.
    let _ = unsafe { libc::kill(-(pgid as libc::pid_t), libc::SIGKILL) };
}

/// No process groups on Windows: kill the recorded process's tree, which
/// needs the root alive to find its descendants.
#[cfg(windows)]
fn kill_recorded_agent_group(pid: u32, leader_alive: bool) {
    if leader_alive {
        let _ = flare_process::command("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .status();
    }
}

/// Force-kills every process whose working directory is inside
/// `worktree_path` (component-wise, so `task/1` never matches `task/10`),
/// other than this process itself.
#[cfg(target_os = "linux")]
fn kill_processes_with_cwd_under(worktree_path: &std::path::Path) {
    let Ok(root) = std::fs::canonicalize(worktree_path) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };
    let me = std::process::id();
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == me {
            continue;
        }
        if std::fs::read_link(entry.path().join("cwd")).is_ok_and(|cwd| cwd.starts_with(&root))
            && !has_controlling_tty(&entry.path())
        {
            let _ = crate::ipc::process::force_kill(pid);
        }
    }
}

/// Whether `/proc/<pid>` has a controlling terminal (`tty_nr`, field 7 of
/// `stat`). Agents the daemon launched never do; a human's shell or editor
/// `cd`'d into the worktree does, and must not be killed by the sweep.
/// Unreadable counts as having one -- fail toward not killing.
#[cfg(target_os = "linux")]
fn has_controlling_tty(proc_dir: &std::path::Path) -> bool {
    std::fs::read_to_string(proc_dir.join("stat"))
        .ok()
        .and_then(|stat| stat_tty_nr(&stat))
        .is_none_or(|tty| tty != 0)
}

/// `tty_nr` from a `/proc/<pid>/stat` line. `comm` (field 2) may contain
/// spaces and parens, so fields are counted from after the last `)`:
/// state, ppid, pgrp, session, tty_nr.
#[cfg(target_os = "linux")]
fn stat_tty_nr(stat: &str) -> Option<i64> {
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(4))
        .and_then(|t| t.parse().ok())
}

/// Swaps `dispatched` back to `ready-for-work` on an item whose in-process
/// job was just reconciled -- the reverse of the label swap
/// `supervisor::dispatch_item` made when the now-dead job was first sent
/// out. Without this, `release_and_comment` above frees the claim but the
/// item still carries `dispatched` (not `ready-for-work`), so
/// `run_discovery_tick` -- which only ever looks at `ready-for-work` -- never
/// sees it again; the item sits stuck until a human relabels it by hand
/// (item #99).
///
/// Also restores `assignee_agent` from the dead job's args: `release_and_comment`
/// clears it via `item_release`, and without putting it back the next discovery
/// tick hits `skip_item`'s "no assignee_agent set" path and lands the item on
/// `needs-manual-dispatch` instead of auto-redispatching (item #150).
///
/// Skips items already in a `completed`/`cancelled` state group -- same
/// exclusion `item::claim`'s handoff-freeze check uses -- so an item a human
/// finished or cancelled out-of-band while its now-dead job was still
/// marked `running` doesn't get silently resurrected back onto the
/// discovery queue.
///
/// Unlike the identical-reason `DISPATCH_FAILURE_CAP` (deliberately not
/// applied here -- a single daemon death mid-job isn't deterministic
/// evidence of a real bug), this DOES apply the looser
/// `DISPATCH_FAILURE_CAP_ANY_REASON`: once an item has racked up that many
/// consecutive non-success dispatch cycles of ANY kind (clean failures,
/// unrecorded orphan-restarts, or a mix), it lands on `needs-manual-dispatch`
/// instead of being unconditionally resurrected onto `ready-for-work` again.
/// Without this, an item whose job keeps orphaning (e.g. across repeated
/// `dev-install` binary-swap restarts) can accumulate hundreds of dispatch
/// cycles with no cap ever engaging, since every orphan-restart cycle resets
/// the identical-reason streak before it can reach 3 (item #164 hit 400+).
/// Returns whether the any-reason cap tripped, so the caller can post the
/// operator-visible cap comment (mirroring `handle_terminal_job_failure`,
/// the only other place that already does).
fn restore_ready_for_work(
    mcp: &crate::mcp_server::AgentflareMcp,
    item_id: &str,
    agent: &str,
    job_id: &str,
) -> bool {
    let at_cap = mcp
        .with_backend_db(|conn| -> Option<bool> {
            let comments = agentflare_backend::comment::list_by_item(conn, item_id).ok()?;
            Some(
                crate::dispatch_failure_ceiling::consecutive_failure_count_any_reason(&comments)
                    >= crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP_ANY_REASON,
            )
        })
        .ok()
        .flatten()
        .unwrap_or(false);

    let _ = mcp.with_backend_db(|conn| -> Option<()> {
        let item = agentflare_backend::item::get(conn, item_id).ok()?;
        let state = agentflare_backend::state::get(conn, &item.state_id).ok()?;
        if matches!(state.group_name.as_str(), "completed" | "cancelled") {
            return None;
        }
        let project = mcp.resolve_project(conn).ok()?;
        let labels = agentflare_backend::label::list_by_project(conn, &project.id).ok()?;
        let dispatched_id = labels
            .iter()
            .find(|l| l.name == crate::supervisor::DISPATCHED_LABEL)
            .map(|l| l.id.clone());

        // Single transaction: a failure partway through must not leave the
        // item labeled ready-for-work with no assignee (item #150) or with
        // `dispatched` still attached (item #99) -- `with_backend_db` gives
        // no rollback of its own, so this crate does it explicitly instead
        // of discarding a mid-sequence error via `.ok()?` and continuing.
        conn.execute_batch("BEGIN IMMEDIATE").ok()?;
        let result: agentflare_backend::error::Result<()> = (|| {
            // Defense in depth: `reconcile_orphaned_jobs` already tries to
            // release this claim via `release_and_comment` under a
            // `with_owner_override` scope before calling here, but that
            // release is best-effort (`let _ = ...`) and its failure is
            // silent. Without this, a release that silently didn't take
            // leaves the claim "live" for its full TTL even after this
            // function puts `ready-for-work` back on -- `run_discovery_tick`
            // sees the item as dispatchable, but the actual dispatch (and
            // `redispatch`) both refuse with "blocked_by_live_claim" until
            // the TTL naturally expires (items #185/#187 reproduced this:
            // stuck for the better part of an hour with no visible error,
            // owner strings confirmed to be this exact job's own dead
            // claim). Same owner string `reconcile_orphaned_jobs` already
            // constructs (`{agent}:{job_id}`) so this only ever releases the
            // dead job's own lease, never a live one held by something else.
            agentflare_backend::claim::release(conn, item_id, &format!("{agent}:{job_id}"))?;
            if let Some(dispatched_id) = &dispatched_id {
                agentflare_backend::item::remove_label(conn, item_id, dispatched_id)?;
            }
            agentflare_backend::item::update(
                conn,
                item_id,
                agentflare_backend::item::UpdateItem {
                    // Prefer the item's own current assignee over this dead
                    // job's frozen payload agent (item #230): a manual
                    // reassignment made after this job was enqueued must
                    // survive reconciliation, not get silently reverted back
                    // to whoever the stale job was originally dispatched to.
                    // Same precedence `item::claim::redispatch` already uses.
                    assignee_agent: Some(
                        item.assignee_agent
                            .clone()
                            .unwrap_or_else(|| agent.to_string()),
                    ),
                    ..Default::default()
                },
            )?;
            if at_cap {
                if let Some(manual_id) = labels
                    .iter()
                    .find(|l| l.name == crate::supervisor::NEEDS_MANUAL_LABEL)
                {
                    agentflare_backend::item::add_label(conn, item_id, &manual_id.id)?;
                }
            } else if let Some(ready_id) = labels
                .iter()
                .find(|l| l.name == crate::supervisor::READY_LABEL)
            {
                agentflare_backend::item::add_label(conn, item_id, &ready_id.id)?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => conn.execute_batch("COMMIT").ok(),
            Err(_) => {
                let _ = conn.execute_batch("ROLLBACK");
                None
            }
        }
    });

    if at_cap
        && crate::supervisor::first_time_gated(item_id)
        && let Ok(Some(item)) =
            mcp.with_backend_db(|conn| agentflare_backend::item::get(conn, item_id).ok())
    {
        crate::supervisor::notify_human_gate(
            &item,
            &format!(
                "dispatch-failure ceiling reached restarting orphaned job {job_id} for agent '{agent}'"
            ),
        );
    }

    at_cap
}

/// Posts the operator-visible `DISPATCH_FAILURE_CAP_MARKER` comment for a
/// trip of the looser `DISPATCH_FAILURE_CAP_ANY_REASON` ceiling. Used by
/// `reconcile_orphaned_jobs`'s orphan-restart path, which -- unlike
/// `handle_terminal_job_failure` -- has no cap-comment step of its own.
fn post_any_reason_cap_comment(mcp: &crate::mcp_server::AgentflareMcp, item_id: &str) {
    let cap = crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP_ANY_REASON;
    let body = format!(
        "{}\n\n{cap} consecutive dispatch cycles failed (mixed or unrecorded reasons, including \
         orphaned/daemon-restart cycles) without a clean success — auto-redispatch stopped. \
         Review the failure comments, fix the underlying issue, then `item action=redispatch` \
         to retry.",
        crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP_MARKER,
    );
    let _ = mcp.comment_impl(crate::mcp_server::types::CommentRequest {
        action: "create".into(),
        item_id: Some(item_id.to_string()),
        body: Some(body),
        ..Default::default()
    });
}

const TERMINAL_FAILURE_RESTORE_ATTEMPTS: usize = 3;

/// `handle_terminal_job_failure`'s label restore, as one `BEGIN IMMEDIATE`
/// transaction: committed only when every step succeeds, rolled back (item
/// untouched, still `dispatched`) on any error. `Ok(None)` means nothing to
/// do (item done/cancelled, project unresolvable, or no label to restore
/// onto); `Ok(Some(at_cap))` otherwise.
fn restore_after_terminal_failure(
    mcp: &crate::mcp_server::AgentflareMcp,
    conn: &rusqlite::Connection,
    item_id: &str,
    agent: &str,
) -> Result<Option<bool>, String> {
    // Resolved before BEGIN, like `restore_ready_for_work`: project
    // resolution may refresh its own link/`project_dirs` bookkeeping, which
    // has no business running inside this transaction.
    let Ok(project) = mcp.resolve_project(conn) else {
        return Ok(None);
    };
    let labels =
        agentflare_backend::label::list_by_project(conn, &project.id).map_err(|e| e.to_string())?;
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(|e| e.to_string())?;
    let result = (|| -> Result<Option<bool>, String> {
        let item = agentflare_backend::item::get(conn, item_id).map_err(|e| e.to_string())?;
        let state =
            agentflare_backend::state::get(conn, &item.state_id).map_err(|e| e.to_string())?;
        if matches!(state.group_name.as_str(), "completed" | "cancelled") {
            return Ok(None);
        }
        let comments =
            agentflare_backend::comment::list_by_item(conn, item_id).map_err(|e| e.to_string())?;
        let find = |name: &str| labels.iter().find(|l| l.name == name).map(|l| &l.id);
        // Stopped on request (workflow cancelled, or paused): drop
        // `dispatched` but never re-arm `ready-for-work`, and never restore
        // `in_review` below either -- a deliberate stop must not be
        // auto-redispatched, nor silently handed back to the review sweep,
        // on the next tick. Must run before the in_review restore below
        // (PR #818 review finding: this used to run after it, so a stop
        // request could be overridden).
        if crate::dispatch_failure_ceiling::stopped_on_request(&comments) {
            if let Some(dispatched_id) = find(crate::supervisor::DISPATCHED_LABEL) {
                agentflare_backend::item::remove_label(conn, item_id, dispatched_id)
                    .map_err(|e| e.to_string())?;
            }
            return Ok(None);
        }
        // Item #655: a repair/retry claim on an item that already had an
        // open PR (`in_review`) unconditionally flips its state to
        // "started" as a side effect of `item::claim` -- and unlike the
        // worktree-creation-failure path (`AgentflareMcp::roll_back_claim`),
        // nothing restores it once the job fails for any other reason.
        // Left alone, `run_review_sweep`'s in_review-only scan permanently
        // loses the item, and its PR sits unwatched no matter how green or
        // approved it later becomes -- a broken dispatch loop (this
        // function's own cap, below) and a stuck-but-fine PR are two
        // unrelated problems, and fixing the human-visible one (the cap
        // comment) must not require separately noticing the other by hand.
        //
        // PR #818 review finding: a human closing the PR without merging
        // while its repair job is in flight means this restores on stale
        // `metadata.pr.number` alone. Left as-is deliberately -- the next
        // `run_review_sweep` tick's per-item PR-status check already covers
        // exactly this case once the item is back in `in_review`
        // (`PrCiStatus::Closed` => `requeue_closed_pr_item`), so the window
        // for an incorrect restore is bounded to one sweep tick. Verifying
        // live here instead would mean a GitHub round trip inside this
        // retried `BEGIN IMMEDIATE` transaction, and there's no test seam
        // for mocking that in this function today (see this PR's own
        // `handle_terminal_job_failure_restores_in_review_for_an_item_with_an_open_pr`,
        // which asserts the restore happens against a plain local repo with
        // no GitHub remote configured).
        //
        // PR #818 review finding: a human can label the item
        // `needs-decision` (a go/no-go gate held pending review, set
        // independently of this job) while its repair job is still in
        // flight. Restoring to `in_review` unconditionally would hand it
        // back to the sweep -- which can merge it -- despite that gate.
        // `needs-manual-dispatch` is deliberately NOT checked here: this
        // function itself only adds that label further below, once the cap
        // trips, so it can never already be set at this point for a job
        // that fails below the cap, and the cap's own single trip must
        // still restore `in_review` (see the comment above `at_cap` below).
        if state.group_name != "in_review"
            && crate::worktree::pr_number_from_metadata(&item).is_some()
            && find(crate::supervisor::NEEDS_DECISION_LABEL).is_none()
        {
            let states = agentflare_backend::state::list_by_project(conn, &project.id)
                .map_err(|e| e.to_string())?;
            if let Some(in_review) = states.iter().find(|s| s.group_name == "in_review") {
                agentflare_backend::item::update_state(conn, item_id, &in_review.id)
                    .map_err(|e| e.to_string())?;
            }
        }
        let identical_count =
            crate::dispatch_failure_ceiling::consecutive_identical_failure_count(&comments);
        let any_reason_count =
            crate::dispatch_failure_ceiling::consecutive_failure_count_any_reason(&comments);
        let at_cap = identical_count >= crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP
            || any_reason_count >= crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP_ANY_REASON;
        let ready_id = find(crate::supervisor::READY_LABEL);
        if !at_cap && ready_id.is_none() {
            // Below cap with no ready-for-work label to restore onto: the
            // only thing left is the removal, same as before.
            if let Some(dispatched_id) = find(crate::supervisor::DISPATCHED_LABEL) {
                agentflare_backend::item::remove_label(conn, item_id, dispatched_id)
                    .map_err(|e| e.to_string())?;
            }
            return Ok(None);
        }

        if let Some(dispatched_id) = find(crate::supervisor::DISPATCHED_LABEL) {
            agentflare_backend::item::remove_label(conn, item_id, dispatched_id)
                .map_err(|e| e.to_string())?;
        }
        // `release_and_comment` already cleared `assignee_agent` via
        // `item_release`. Restore it in both branches: below cap, the next
        // discovery tick would otherwise hit `skip_item` (item #150); at
        // cap, the cap comment's own `item action=redispatch` instruction
        // needs an existing `assignee_agent` unless one is passed
        // explicitly. Prefer the item's own current assignee over this
        // dead job's frozen payload agent (item #230) -- see the matching
        // comment in `restore_ready_for_work` above.
        agentflare_backend::item::update(
            conn,
            item_id,
            agentflare_backend::item::UpdateItem {
                assignee_agent: Some(
                    item.assignee_agent
                        .clone()
                        .unwrap_or_else(|| agent.to_string()),
                ),
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?;
        if at_cap {
            // No `needs-manual-dispatch` label on the project: leave the
            // item off `ready-for-work` -- the cap comment posted by the
            // caller is what a human/PM acts on.
            if let Some(manual_id) = find(crate::supervisor::NEEDS_MANUAL_LABEL) {
                agentflare_backend::item::add_label(conn, item_id, manual_id)
                    .map_err(|e| e.to_string())?;
            }
        } else if let Some(ready_id) = ready_id {
            agentflare_backend::item::add_label(conn, item_id, ready_id)
                .map_err(|e| e.to_string())?;
        }
        Ok(Some(at_cap))
    })();
    match result {
        Ok(outcome) => match conn.execute_batch("COMMIT") {
            Ok(()) => Ok(outcome),
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                Err(e.to_string())
            }
        },
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

/// Registered as the daemon's `WorkerPool::with_terminal_failure_hook` (see
/// `dashboard::server::run`) -- fires when an `in_process` job reaches
/// terminal `state = 'failed'` after exhausting its retries, the clean-
/// failure counterpart to `reconcile_orphaned_jobs` above (which only
/// catches a job whose *process* died mid-flight). `execute_work` already
/// released the item's claim and posted a failure comment on its own last
/// attempt (`cli::work::release_and_comment`) -- the one thing still
/// missing is undoing `dispatch_item`'s `ready-for-work` -> `dispatched`
/// label swap. Left alone the item stays labeled `dispatched` forever,
/// invisible to `run_discovery_tick` (item #463).
///
/// Below `dispatch_failure_ceiling::DISPATCH_FAILURE_CAP` consecutive
/// dispatch cycles with the same terminal failure reason, or
/// `DISPATCH_FAILURE_CAP_ANY_REASON` consecutive non-success cycles of any
/// kind (one cycle per `## supervisor — dispatched` comment — intra-job
/// retries within a cycle do not increment either count; see that module),
/// swaps back to `ready-for-work` so a transient failure can auto-redispatch
/// on the next discovery tick. At or above either cap, stops auto-dispatch:
/// lands on `needs-manual-dispatch` when that label exists on the project,
/// otherwise leaves the item off `ready-for-work` with a supervisor cap
/// comment so a human/PM must intervene (`item action=redispatch` after
/// fixing the root cause). Orphan-restart recovery (`restore_ready_for_work`
/// above) deliberately does not apply the *identical-reason* cap — a single
/// daemon death mid-job is not evidence of a deterministic failure class —
/// but it does apply the looser any-reason cap, so an item whose job keeps
/// orphaning across repeated daemon restarts still gets stopped eventually
/// (item #164, which racked up 400+ dispatch cycles before this existed).
pub(super) fn handle_terminal_job_failure(job: &agentflare_jobs::AgentJob) {
    if !job.in_process {
        return;
    }
    let (Some(item_id), Some(agent)) = (job.args.first(), job.args.get(1)) else {
        return;
    };
    let mcp = match job.args.get(2) {
        Some(folder_path) => {
            crate::mcp_server::AgentflareMcp::for_project_dir(std::path::PathBuf::from(folder_path))
        }
        None => crate::mcp_server::AgentflareMcp::default(),
    };
    // One IMMEDIATE transaction around the whole label swap, same reasoning
    // as `restore_ready_for_work` above: `dispatched` coming off and
    // `ready-for-work`/`needs-manual-dispatch` going on must land together.
    // Done piecemeal (as this used to be), a failure after the removal --
    // most often a busy database -- left the item with neither label,
    // invisible to `run_discovery_tick` forever. A failed attempt rolls
    // back to the untouched `dispatched` state and is retried a couple of
    // times, since this hook fires exactly once per terminal job.
    let cap_reached = mcp.with_backend_db(|conn| -> Option<bool> {
        let mut last_err = None;
        for _ in 0..TERMINAL_FAILURE_RESTORE_ATTEMPTS {
            match restore_after_terminal_failure(&mcp, conn, item_id, agent) {
                Ok(outcome) => return outcome,
                Err(e) => last_err = Some(e),
            }
        }
        if let Some(e) = last_err {
            eprintln!(
                "agentflare-supervisor: restoring item {item_id} after terminal job failure \
                 failed and was rolled back (still labeled {}): {e}",
                crate::supervisor::DISPATCHED_LABEL
            );
        }
        None
    });

    let Ok(Some(true)) = cap_reached else {
        return;
    };

    let (reason_preview, identical_count, any_reason_count) = mcp
        .with_backend_db(|conn| {
            let comments = agentflare_backend::comment::list_by_item(conn, item_id).ok()?;
            Some((
                crate::dispatch_failure_ceiling::latest_failure_reason(&comments),
                crate::dispatch_failure_ceiling::consecutive_identical_failure_count(&comments),
                crate::dispatch_failure_ceiling::consecutive_failure_count_any_reason(&comments),
            ))
        })
        .ok()
        .flatten()
        .unwrap_or((None, 0, 0));

    let identical_cap = crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP;
    let mut body = if identical_count >= identical_cap {
        format!(
            "{}\n\n{identical_cap} consecutive identical failures detected — auto-redispatch \
             stopped. Review the failure comments, fix the underlying issue, then \
             `item action=redispatch` to retry.",
            crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP_MARKER,
        )
    } else {
        format!(
            "{}\n\n{any_reason_count} consecutive dispatch cycles failed (mixed or unrecorded \
             reasons) without a clean success — auto-redispatch stopped. Review the failure \
             comments, fix the underlying issue, then `item action=redispatch` to retry.",
            crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP_MARKER,
        )
    };
    if let Some(reason) = reason_preview {
        body.push_str(&format!("\n\nLast failure: `{reason}`"));
    }
    let _ = mcp.comment_impl(crate::mcp_server::types::CommentRequest {
        action: "create".into(),
        item_id: Some(item_id.clone()),
        body: Some(body),
        ..Default::default()
    });

    if crate::supervisor::first_time_gated(item_id)
        && let Ok(Some(item)) =
            mcp.with_backend_db(|conn| agentflare_backend::item::get(conn, item_id).ok())
    {
        crate::supervisor::notify_human_gate(
            &item,
            "dispatch-failure ceiling reached — auto-redispatch stopped",
        );
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn stat_tty_nr_parses_past_a_comm_with_spaces_and_parens() {
        let detached = "4242 (claude (x) y) S 1 4242 4242 0 -1 4194560 0";
        let interactive = "77 (bash) S 70 77 77 34816 77 4194560 0";
        assert_eq!(super::stat_tty_nr(detached), Some(0));
        assert_eq!(super::stat_tty_nr(interactive), Some(34816));
    }

    include!("orphan_reconcile_tests.rs");
    include!("orphan_reconcile_assignee_tests.rs");
    include!("orphan_reconcile_failover_tests.rs");

    /// A failure partway through the terminal-failure label swap must roll
    /// the whole swap back, never strand the item with neither `dispatched`
    /// nor `ready-for-work` (invisible to discovery forever) -- and once the
    /// fault clears, the next run must complete the swap.
    #[test]
    fn handle_terminal_job_failure_is_all_or_nothing_when_a_step_fails() {
        crate::paths::test_support::with_temp_home(|| {
            let tmp = tempfile::tempdir().unwrap();
            let repo_root = tmp.path().join("repo");
            std::fs::create_dir_all(&repo_root).unwrap();
            init_test_repo(&repo_root);

            let mcp = crate::mcp_server::AgentflareMcp::for_project_dir(repo_root.clone());
            let label_ids = seed_labels(
                &mcp,
                &[
                    crate::supervisor::READY_LABEL,
                    crate::supervisor::DISPATCHED_LABEL,
                ],
            );
            let dispatched_id = &label_ids[crate::supervisor::DISPATCHED_LABEL];
            let ready_id = &label_ids[crate::supervisor::READY_LABEL];
            let item_id = create_dispatched_item(&mcp, dispatched_id);

            // Fault injection: the final `ready-for-work` add fails, after
            // `dispatched` has already been removed within the same run.
            mcp.with_backend_db(|conn| {
                conn.execute_batch(&format!(
                    "CREATE TRIGGER fail_ready_add BEFORE INSERT ON item_labels \
                     WHEN NEW.label_id = '{ready_id}' \
                     BEGIN SELECT RAISE(ABORT, 'injected failure'); END;"
                ))
                .unwrap();
            })
            .unwrap();

            let job = agentflare_jobs::AgentJob::new("agentflare-work")
                .args([
                    item_id.clone(),
                    "claude-code".to_string(),
                    repo_root.to_string_lossy().to_string(),
                ])
                .in_process();
            handle_terminal_job_failure(&job);

            let labels = mcp
                .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id))
                .unwrap()
                .unwrap();
            assert!(
                labels.contains(dispatched_id),
                "a failed swap must roll back to `dispatched`, not drop both labels: {labels:?}"
            );
            assert!(!labels.contains(ready_id));

            mcp.with_backend_db(|conn| conn.execute_batch("DROP TRIGGER fail_ready_add").unwrap())
                .unwrap();
            handle_terminal_job_failure(&job);

            let labels = mcp
                .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id))
                .unwrap()
                .unwrap();
            assert!(labels.contains(ready_id), "{labels:?}");
            assert!(!labels.contains(dispatched_id), "{labels:?}");
        });
    }
}
