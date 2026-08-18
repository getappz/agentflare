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
        let owner = format!("{agent}:{job_id}");
        crate::claims::with_owner_override(owner, || {
            crate::cli::work::release_and_comment(
                &mcp,
                item_id,
                "orphaned by daemon restart",
                None,
            );
        });
        restore_ready_for_work(&mcp, item_id, agent);
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
fn restore_ready_for_work(
    mcp: &crate::mcp_server::AgentflareMcp,
    item_id: &str,
    agent: &str,
) {
    let _ = mcp.with_backend_db(|conn| -> Option<()> {
        let item = agentflare_backend::item::get(conn, item_id).ok()?;
        let state = agentflare_backend::state::get(conn, &item.state_id).ok()?;
        if matches!(state.group_name.as_str(), "completed" | "cancelled") {
            return None;
        }
        let project = mcp.resolve_project(conn).ok()?;
        let labels = agentflare_backend::label::list_by_project(conn, &project.id).ok()?;
        let ready_id = &labels
            .iter()
            .find(|l| l.name == crate::supervisor::READY_LABEL)?
            .id;
        if let Some(dispatched_id) = labels
            .iter()
            .find(|l| l.name == crate::supervisor::DISPATCHED_LABEL)
        {
            let _ = agentflare_backend::item::remove_label(conn, item_id, &dispatched_id.id);
        }
        agentflare_backend::item::add_label(conn, item_id, ready_id).ok()?;
        agentflare_backend::item::update(
            conn,
            item_id,
            agentflare_backend::item::UpdateItem {
                assignee_agent: Some(agent.to_string()),
                ..Default::default()
            },
        )
        .ok()?;
        Some(())
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
/// Swaps to `needs-manual-dispatch` rather than back to `ready-for-work`
/// when that label exists on the project: a clean retry-exhaustion is more
/// often an unretryable problem (bad credentials, no billing balance) than
/// a transient one, and blindly re-queueing would just retry-loop against
/// the same broken agent. Falls back to `ready-for-work` when
/// `needs-manual-dispatch` hasn't been created for this project (unlike
/// that label, `ready-for-work` is guaranteed to exist -- a project can
/// only ever have reached `dispatch_item` by already having it) so the item
/// never ends up worse off than before this hook existed: still stuck, just
/// unlabeled.
pub(super) fn handle_terminal_job_failure(job: &agentflare_jobs::AgentJob) {
    if !job.in_process {
        return;
    }
    let (Some(item_id), Some(_agent)) = (job.args.first(), job.args.get(1)) else {
        return;
    };
    let mcp = match job.args.get(2) {
        Some(folder_path) => {
            crate::mcp_server::AgentflareMcp::for_project_dir(std::path::PathBuf::from(folder_path))
        }
        None => crate::mcp_server::AgentflareMcp::default(),
    };
    let _ = mcp.with_backend_db(|conn| -> Option<()> {
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
        let target_id = &labels
            .iter()
            .find(|l| l.name == crate::supervisor::NEEDS_MANUAL_LABEL)
            .or_else(|| {
                labels
                    .iter()
                    .find(|l| l.name == crate::supervisor::READY_LABEL)
            })?
            .id;
        agentflare_backend::item::add_label(conn, item_id, target_id).ok()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentflare_jobs::JobState;

    fn init_test_repo(root: &std::path::Path) {
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap();
        };
        run(&["init", "-b", "master"]);
        run(&["config", "user.email", "test@test.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(root.join("README.md"), "test\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-m", "initial"]);
    }

    fn test_queue() -> Queue {
        // `.keep()` so the dir outlives this function — otherwise the
        // returned `Queue`'s `log_dir` would point at an already-deleted
        // path (harmless for tests that never touch logs, but a footgun for
        // ones that do).
        let dir = tempfile::tempdir().unwrap().keep();
        Queue::open_memory(dir.join("logs")).unwrap()
    }

    /// End-to-end (item #40): a job dispatched by `enqueue_work_job`
    /// (`supervisor.rs`) is claimed, its `agent_jobs` row moves to
    /// `running`, and then the process dies before finishing -- simulated
    /// here by never calling `queue.complete`/`fail` and instead going
    /// straight to `reconcile_orphaned_jobs`, exactly what daemon startup
    /// does on the next run. Confirms the row ends terminal *and* the
    /// item's claim is actually released (re-claimable), not just that the
    /// DB row changed state.
    #[test]
    fn reconcile_orphaned_jobs_releases_the_dead_jobs_item_claim() {
        crate::paths::test_support::with_temp_home(|| {
            let tmp = tempfile::tempdir().unwrap();
            let repo_root = tmp.path().join("repo");
            std::fs::create_dir_all(&repo_root).unwrap();
            init_test_repo(&repo_root);

            let mcp = crate::mcp_server::AgentflareMcp::for_project_dir(repo_root.clone());
            let item = mcp
                .with_backend_db(|conn| {
                    let project = mcp.resolve_project(conn).unwrap();
                    let state = agentflare_backend::state::list_by_project(conn, &project.id)
                        .unwrap()
                        .into_iter()
                        .find(|s| s.is_default)
                        .unwrap();
                    agentflare_backend::item::create(
                        conn,
                        agentflare_backend::item::CreateItem {
                            project_id: project.id,
                            state_id: state.id,
                            name: "orphan sweep test item".into(),
                            description: Some("do the thing".into()),
                            priority: None,
                            parent_id: None,
                            assignee_agent: None,
                            sort_order: None,
                            external_source: None,
                            external_id: None,
                            metadata: None,
                            label_ids: vec![],
                            assignee_ids: vec![],
                            dependency_ids: vec![],
                        },
                    )
                    .unwrap()
                })
                .unwrap();

            // Claim it as `claude-code:<job-id>` -- the same owner shape
            // `WorkItemExecutor::execute` uses -- so `reconcile_orphaned_jobs`
            // is releasing the exact claim its owner-string reconstruction
            // is meant to match, not a fresh claim of its own.
            let job = agentflare_jobs::AgentJob::new("agentflare-work")
                .args([
                    item.id.clone(),
                    "claude-code".to_string(),
                    repo_root.to_string_lossy().to_string(),
                ])
                .in_process();
            let queue = test_queue();
            let info = queue.enqueue(&job).unwrap();
            crate::claims::with_owner_override(format!("claude-code:{}", info.id), || {
                let claim_json = mcp
                    .item_claim(crate::mcp_server::types::ItemRequest {
                        action: "claim".to_string(),
                        id: Some(item.id.clone()),
                        ..Default::default()
                    })
                    .unwrap();
                let claim: serde_json::Value = serde_json::from_str(&claim_json).unwrap();
                assert_eq!(claim["status"], "acquired");
            });

            // Simulate the daemon dying mid-job: the row is `running` with
            // no completion ever recorded.
            queue.dequeue().unwrap();

            reconcile_orphaned_jobs(&queue);

            let reconciled = queue.get(&info.id).unwrap();
            assert_eq!(reconciled.state, JobState::Failed);
            assert_eq!(
                reconciled.error.as_deref(),
                Some("orphaned by daemon restart")
            );

            // The real point of this test: the claim must actually be gone,
            // not just the job row updated -- a fresh same-agent-type claim
            // (simulating re-dispatch after the restart) must succeed.
            // Explicit `with_owner_override`, not ambient env/pid resolution
            // -- the latter is environment-dependent and flaked in CI.
            // Cross-agent-type would legitimately hit BlockedByAssignee
            // (protects a still-open handoff) -- not what this test covers.
            let reclaim_json =
                crate::claims::with_owner_override("claude-code:a-fresh-instance", || {
                    mcp.item_claim(crate::mcp_server::types::ItemRequest {
                        action: "claim".to_string(),
                        id: Some(item.id.clone()),
                        ..Default::default()
                    })
                    .unwrap()
                });
            let reclaim: serde_json::Value = serde_json::from_str(&reclaim_json).unwrap();
            assert_eq!(
                reclaim["status"], "acquired",
                "orphaned job's claim must be released so the item is re-claimable: {reclaim}"
            );

            let comments = mcp
                .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item.id))
                .unwrap()
                .unwrap();
            assert_eq!(comments.len(), 1);
            assert!(comments[0].body.contains("orphaned by daemon restart"));
        });
    }

    /// Item #99: releasing the dead job's claim isn't enough on its own --
    /// `supervisor::dispatch_item` had already swapped the item's label from
    /// `ready-for-work` to `dispatched` before the job died, and
    /// `run_discovery_tick` only ever looks at `ready-for-work`. Without
    /// restoring the label, the item is claim-free but permanently invisible
    /// to auto-dispatch. This test seeds the item with both labels present
    /// (mirroring how a real project has them) and the item already carrying
    /// `dispatched`, then confirms reconciliation swaps it back.
    #[test]
    fn reconcile_orphaned_jobs_restores_ready_for_work_label() {
        crate::paths::test_support::with_temp_home(|| {
            let tmp = tempfile::tempdir().unwrap();
            let repo_root = tmp.path().join("repo");
            std::fs::create_dir_all(&repo_root).unwrap();
            init_test_repo(&repo_root);

            let mcp = crate::mcp_server::AgentflareMcp::for_project_dir(repo_root.clone());
            let (item_id, dispatched_label_id) = mcp
                .with_backend_db(|conn| {
                    let project = mcp.resolve_project(conn).unwrap();
                    for name in [
                        crate::supervisor::READY_LABEL,
                        crate::supervisor::DISPATCHED_LABEL,
                    ] {
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
                    let state = agentflare_backend::state::list_by_project(conn, &project.id)
                        .unwrap()
                        .into_iter()
                        .find(|s| s.is_default)
                        .unwrap();
                    let item = agentflare_backend::item::create(
                        conn,
                        agentflare_backend::item::CreateItem {
                            project_id: project.id.clone(),
                            state_id: state.id,
                            name: "orphan sweep relabel test item".into(),
                            description: Some("do the thing".into()),
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
                    let labels =
                        agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
                    let dispatched_id = labels
                        .iter()
                        .find(|l| l.name == crate::supervisor::DISPATCHED_LABEL)
                        .unwrap()
                        .id
                        .clone();
                    // Mirrors `dispatch_item`'s own label swap: `dispatched`
                    // on, `ready-for-work` never added (it was removed at
                    // dispatch time in the real flow).
                    agentflare_backend::item::add_label(conn, &item.id, &dispatched_id).unwrap();
                    (item.id, dispatched_id)
                })
                .unwrap();

            let job = agentflare_jobs::AgentJob::new("agentflare-work")
                .args([
                    item_id.clone(),
                    "claude-code".to_string(),
                    repo_root.to_string_lossy().to_string(),
                ])
                .in_process();
            let queue = test_queue();
            let info = queue.enqueue(&job).unwrap();
            crate::claims::with_owner_override(format!("claude-code:{}", info.id), || {
                let claim_json = mcp
                    .item_claim(crate::mcp_server::types::ItemRequest {
                        action: "claim".to_string(),
                        id: Some(item_id.clone()),
                        ..Default::default()
                    })
                    .unwrap();
                let claim: serde_json::Value = serde_json::from_str(&claim_json).unwrap();
                assert_eq!(claim["status"], "acquired");
            });

            // Daemon dies mid-job: the row is left `running`.
            queue.dequeue().unwrap();

            reconcile_orphaned_jobs(&queue);

            let labels = mcp
                .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id))
                .unwrap()
                .unwrap();
            let ready_id = mcp
                .with_backend_db(|conn| {
                    let project = mcp.resolve_project(conn).unwrap();
                    agentflare_backend::label::list_by_project(conn, &project.id)
                        .unwrap()
                        .into_iter()
                        .find(|l| l.name == crate::supervisor::READY_LABEL)
                        .unwrap()
                        .id
                })
                .unwrap();
            assert!(
                labels.contains(&ready_id),
                "orphan reconciliation must restore ready-for-work so the item auto-resumes (item #99)"
            );
            let item = mcp
                .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id))
                .unwrap()
                .unwrap();
            assert_eq!(
                item.assignee_agent.as_deref(),
                Some("claude-code"),
                "orphan reconciliation must restore assignee_agent so discovery can auto-dispatch (item #150)"
            );
            assert!(
                !labels.contains(&dispatched_label_id),
                "the stale dispatched label must be removed, not left alongside ready-for-work"
            );
        });
    }

    /// Item #150: after orphan reconciliation the item must still be
    /// auto-dispatchable -- `skip_item`'s "no assignee_agent set" path must
    /// not fire on the next discovery tick.
    #[test]
    fn reconcile_orphaned_jobs_leaves_item_dispatchable_by_discovery_tick() {
        crate::paths::test_support::with_temp_home(|| {
            let tmp = tempfile::tempdir().unwrap();
            let repo_root = tmp.path().join("repo");
            std::fs::create_dir_all(&repo_root).unwrap();
            init_test_repo(&repo_root);

            let mcp = crate::mcp_server::AgentflareMcp::for_project_dir(repo_root.clone());
            let item_id = mcp
                .with_backend_db(|conn| {
                    let project = mcp.resolve_project(conn).unwrap();
                    for name in [
                        crate::supervisor::READY_LABEL,
                        crate::supervisor::DISPATCHED_LABEL,
                        crate::supervisor::NEEDS_MANUAL_LABEL,
                    ] {
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
                    let state = agentflare_backend::state::list_by_project(conn, &project.id)
                        .unwrap()
                        .into_iter()
                        .find(|s| s.is_default)
                        .unwrap();
                    let item = agentflare_backend::item::create(
                        conn,
                        agentflare_backend::item::CreateItem {
                            project_id: project.id.clone(),
                            state_id: state.id,
                            name: "orphan sweep discovery tick test item".into(),
                            description: Some("do the thing".into()),
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
                    let labels =
                        agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
                    let dispatched_id = labels
                        .iter()
                        .find(|l| l.name == crate::supervisor::DISPATCHED_LABEL)
                        .unwrap()
                        .id
                        .clone();
                    agentflare_backend::item::add_label(conn, &item.id, &dispatched_id).unwrap();
                    item.id
                })
                .unwrap();

            let job = agentflare_jobs::AgentJob::new("agentflare-work")
                .args([
                    item_id.clone(),
                    "claude-code".to_string(),
                    repo_root.to_string_lossy().to_string(),
                ])
                .in_process();
            let queue = test_queue();
            let info = queue.enqueue(&job).unwrap();
            crate::claims::with_owner_override(format!("claude-code:{}", info.id), || {
                let claim_json = mcp
                    .item_claim(crate::mcp_server::types::ItemRequest {
                        action: "claim".to_string(),
                        id: Some(item_id.clone()),
                        ..Default::default()
                    })
                    .unwrap();
                let claim: serde_json::Value = serde_json::from_str(&claim_json).unwrap();
                assert_eq!(claim["status"], "acquired");
            });
            queue.dequeue().unwrap();

            reconcile_orphaned_jobs(&queue);

            let auth_conn = rusqlite::Connection::open_in_memory().unwrap();
            crate::auth_db::migrate(&auth_conn).unwrap();
            let result = crate::supervisor::run_discovery_tick(
                &mcp,
                &queue,
                &auth_conn,
                agentflare_resource_gate::Policy::Normal,
            );
            assert_eq!(
                result.dispatched, 1,
                "freshly-reconciled item must auto-dispatch, not hit skip_item (item #150)"
            );
            assert_eq!(result.skipped, 0);
        });
    }

    fn seed_labels(
        mcp: &crate::mcp_server::AgentflareMcp,
        names: &[&str],
    ) -> std::collections::HashMap<String, String> {
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            for name in names {
                agentflare_backend::label::create(
                    conn,
                    agentflare_backend::label::CreateLabel {
                        project_id: Some(project.id.clone()),
                        workspace_id: project.workspace_id.clone(),
                        name: (*name).into(),
                        color: None,
                        parent_id: None,
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                    },
                )
                .unwrap();
            }
            agentflare_backend::label::list_by_project(conn, &project.id)
                .unwrap()
                .into_iter()
                .map(|l| (l.name, l.id))
                .collect()
        })
        .unwrap()
    }

    fn create_dispatched_item(
        mcp: &crate::mcp_server::AgentflareMcp,
        dispatched_id: &str,
    ) -> String {
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            let state = agentflare_backend::state::list_by_project(conn, &project.id)
                .unwrap()
                .into_iter()
                .find(|s| s.is_default)
                .unwrap();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id,
                    state_id: state.id,
                    name: "clean-failure sweep test item".into(),
                    description: Some("do the thing".into()),
                    priority: None,
                    parent_id: None,
                    assignee_agent: None,
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
            // Mirrors `dispatch_item`'s own label swap: `dispatched` on,
            // `ready-for-work` never added (removed at dispatch time in the
            // real flow).
            agentflare_backend::item::add_label(conn, &item.id, dispatched_id).unwrap();
            item.id
        })
        .unwrap()
    }

    /// Item #463: a job that runs to completion and cleanly fails after
    /// exhausting `max_retries` is *not* orphaned (the process never died),
    /// so `reconcile_orphaned_jobs` above never sees it. Without this hook
    /// the item stays labeled `dispatched` forever -- invisible to
    /// `run_discovery_tick`, silently undispatchable. Confirms
    /// `handle_terminal_job_failure` swaps `dispatched` for
    /// `needs-manual-dispatch` (not straight back to `ready-for-work`) when
    /// that label exists, so a broken agent doesn't just retry-loop.
    #[test]
    fn handle_terminal_job_failure_swaps_dispatched_for_needs_manual_dispatch() {
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
                    crate::supervisor::NEEDS_MANUAL_LABEL,
                ],
            );
            let dispatched_id = &label_ids[crate::supervisor::DISPATCHED_LABEL];
            let item_id = create_dispatched_item(&mcp, dispatched_id);

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
                labels.contains(&label_ids[crate::supervisor::NEEDS_MANUAL_LABEL]),
                "a clean retry-exhaustion must land on needs-manual-dispatch, not silently vanish (item #463)"
            );
            assert!(
                !labels.contains(dispatched_id),
                "the stale dispatched label must be removed"
            );
            assert!(
                !labels.contains(&label_ids[crate::supervisor::READY_LABEL]),
                "needs-manual-dispatch exists on this project, so ready-for-work is the wrong \
                 target -- it would just retry-loop against the same broken agent"
            );
        });
    }

    /// Same clean-failure path as above, but the project never created a
    /// `needs-manual-dispatch` label (that label is only ever added by hand
    /// -- unlike `ready-for-work`/`dispatched`, nothing seeds it). Falling
    /// back to `ready-for-work` keeps the item dispatchable instead of
    /// leaving it stuck exactly as before this hook existed.
    #[test]
    fn handle_terminal_job_failure_falls_back_to_ready_for_work_without_needs_manual_label() {
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
            let item_id = create_dispatched_item(&mcp, dispatched_id);

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
                labels.contains(&label_ids[crate::supervisor::READY_LABEL]),
                "with no needs-manual-dispatch label on the project, the item must still \
                 recover to ready-for-work rather than staying stuck on dispatched"
            );
            assert!(!labels.contains(dispatched_id));
        });
    }

    /// A plain subprocess job (`POST /api/jobs`) has no work item behind it
    /// at all -- `job.in_process` is the same guard `reconcile_orphaned_jobs`
    /// uses to skip those. Confirms the hook is a no-op for one rather than
    /// misinterpreting `args[0]` as an item id.
    #[test]
    fn handle_terminal_job_failure_ignores_non_in_process_jobs() {
        crate::paths::test_support::with_temp_home(|| {
            let job = agentflare_jobs::AgentJob::new("some-subprocess-cmd")
                .args(["not-an-item-id".to_string()]);
            // No project/backend set up at all -- if this tried to resolve
            // one it would panic or error; reaching the end without doing so
            // proves it returned early on the `in_process` guard.
            handle_terminal_job_failure(&job);
        });
    }
}
