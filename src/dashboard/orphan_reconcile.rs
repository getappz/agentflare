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
#[cfg(unix)]
fn kill_processes_touching_worktree(worktree_path: &std::path::Path) {
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
#[cfg(windows)]
fn kill_processes_touching_worktree(worktree_path: &std::path::Path) {
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
        conn.execute_batch("BEGIN").ok()?;
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
                    assignee_agent: Some(agent.to_string()),
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
    let cap_reached = mcp.with_backend_db(|conn| -> Option<bool> {
        let item = agentflare_backend::item::get(conn, item_id).ok()?;
        let state = agentflare_backend::state::get(conn, &item.state_id).ok()?;
        if matches!(state.group_name.as_str(), "completed" | "cancelled") {
            return None;
        }
        let project = mcp.resolve_project(conn).ok()?;
        let labels = agentflare_backend::label::list_by_project(conn, &project.id).ok()?;
        if let Some(dispatched_id) = labels
            .iter()
            .find(|l| l.name == crate::supervisor::DISPATCHED_LABEL)
        {
            let _ = agentflare_backend::item::remove_label(conn, item_id, &dispatched_id.id);
        }

        let comments = agentflare_backend::comment::list_by_item(conn, item_id).ok()?;
        let identical_count =
            crate::dispatch_failure_ceiling::consecutive_identical_failure_count(&comments);
        let any_reason_count =
            crate::dispatch_failure_ceiling::consecutive_failure_count_any_reason(&comments);
        let at_cap = identical_count >= crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP
            || any_reason_count >= crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP_ANY_REASON;

        if at_cap {
            // Same restoration as the below-cap branch: `release_and_comment`
            // already cleared `assignee_agent` via `item_release`. The cap
            // comment tells a human to run `item action=redispatch`, which
            // requires an existing `assignee_agent` unless one is passed
            // explicitly — leaving it cleared here would make that
            // instruction fail.
            agentflare_backend::item::update(
                conn,
                item_id,
                agentflare_backend::item::UpdateItem {
                    assignee_agent: Some(agent.clone()),
                    ..Default::default()
                },
            )
            .ok();
            if let Some(manual_id) = labels
                .iter()
                .find(|l| l.name == crate::supervisor::NEEDS_MANUAL_LABEL)
            {
                agentflare_backend::item::add_label(conn, item_id, &manual_id.id).ok();
            }
            Some(true)
        } else if let Some(ready_id) = labels
            .iter()
            .find(|l| l.name == crate::supervisor::READY_LABEL)
        {
            // `release_and_comment` clears assignee via `item_release`; without
            // restoring it the next discovery tick hits `skip_item` (item
            // #150). Restore it before adding the label so a mid-failure here
            // never leaves the item labeled ready-for-work with no assignee.
            agentflare_backend::item::update(
                conn,
                item_id,
                agentflare_backend::item::UpdateItem {
                    assignee_agent: Some(agent.clone()),
                    ..Default::default()
                },
            )
            .ok()?;
            agentflare_backend::item::add_label(conn, item_id, &ready_id.id).ok();
            Some(false)
        } else {
            None
        }
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
}

#[cfg(test)]
mod tests {
    include!("orphan_reconcile_tests.rs");
}
