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
                            start_date: None,
                            due_date: None,
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

    /// Items #185/#187: `reconcile_orphaned_jobs`'s earlier release (via
    /// `release_and_comment` under `with_owner_override`) is best-effort
    /// (`let _ = ...`) -- if it silently doesn't take, `restore_ready_for_work`
    /// must not leave the claim behind just because it already re-added
    /// `ready-for-work`. Calls `restore_ready_for_work` directly, skipping
    /// the earlier release step entirely, to prove its own defensive release
    /// closes the gap standalone rather than relying on the first release
    /// having worked.
    #[test]
    fn restore_ready_for_work_releases_the_claim_even_when_the_earlier_release_was_skipped() {
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
                            name: "restore-without-prior-release test item".into(),
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
                            start_date: None,
                            due_date: None,
                        },
                    )
                    .unwrap()
                })
                .unwrap();

            let job_id = "a-dead-job-id";
            crate::claims::with_owner_override(format!("opencode:{job_id}"), || {
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

            // Straight to `restore_ready_for_work` -- no `release_and_comment`
            // call first, simulating that step having silently no-op'd.
            restore_ready_for_work(&mcp, &item.id, "opencode", job_id);

            let reclaim_json =
                crate::claims::with_owner_override("opencode:a-fresh-instance", || {
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
                "restore_ready_for_work must release the dead job's own claim itself, \
                 not depend on an earlier release having already succeeded: {reclaim}"
            );
        });
    }

    /// Item #164: an orphaned subprocess must actually be killed, not just
    /// have its DB row updated. Spawns a real long-lived process whose argv
    /// embeds the worktree path (mirroring a real sandboxed agent's bwrap
    /// invocation) and confirms reconciliation terminates it. Unix-only:
    /// `yes` is spawned directly (no shell) so its argv genuinely,
    /// permanently contains the path -- a shell one-liner risks tail-call
    /// execing away the embedded path. Windows' branch is exercised
    /// manually, not by a test here.
    #[test]
    #[cfg(unix)]
    fn reconcile_orphaned_jobs_kills_a_still_alive_orphaned_process() {
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
                            name: "orphan process kill test item".into(),
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
                            start_date: None,
                            due_date: None,
                        },
                    )
                    .unwrap()
                })
                .unwrap();

            let worktree_path = repo_root
                .join(".worktrees")
                .join("task")
                .join(item.sequence_id.to_string());
            std::fs::create_dir_all(&worktree_path).unwrap();

            // A real, long-lived orphan whose argv embeds the worktree path,
            // exactly what `pgrep -f <path>` needs to find it -- mirrors how
            // a real bwrap-sandboxed agent CLI's command line embeds its
            // `--bind <worktree_path> <worktree_path>` argument. Spawned
            // directly (no shell) so there's no risk of the shell's
            // single-command exec-optimization silently replacing this
            // process's argv with something that no longer contains the
            // path (as `sh -c "sleep 30 # <path>"` does in practice --
            // `sh` tail-calls straight into `sleep 30`, dropping the
            // comment from `/proc/<pid>/cmdline` entirely). `yes` accepts
            // any argument uncritically and loops forever printing it.
            let mut orphan = std::process::Command::new("yes")
                .arg(worktree_path.display().to_string())
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap();
            let orphan_pid = orphan.id();

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
                mcp.item_claim(crate::mcp_server::types::ItemRequest {
                    action: "claim".to_string(),
                    id: Some(item.id.clone()),
                    ..Default::default()
                })
                .unwrap();
            });
            queue.dequeue().unwrap();

            reconcile_orphaned_jobs(&queue);

            // Give the kill a moment to land, then confirm the orphan is
            // actually gone -- `try_wait` returns `Ok(Some(_))` once the
            // process has exited.
            for _ in 0..50 {
                if matches!(orphan.try_wait(), Ok(Some(_))) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            assert!(
                matches!(orphan.try_wait(), Ok(Some(_))),
                "orphaned process (pid {orphan_pid}) touching the item's worktree must be \
                 killed during reconciliation, not left running to race a fresh dispatch"
            );
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
                            start_date: None,
                            due_date: None,
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

    /// Item #164: an item whose job orphans repeatedly (e.g. across
    /// `dev-install` daemon restarts) mixed with clean failures of varying
    /// reasons never accumulates 3 *identical* consecutive failures, so
    /// `DISPATCH_FAILURE_CAP` alone never trips — this pins that the looser
    /// `DISPATCH_FAILURE_CAP_ANY_REASON` still stops it once enough
    /// non-success cycles pile up, instead of resurrecting it onto
    /// `ready-for-work` forever.
    #[test]
    fn reconcile_orphaned_jobs_caps_after_repeated_mixed_failures() {
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
            let item_id = mcp
                .with_backend_db(|conn| {
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
                            name: "mixed-failure orphan sweep test item".into(),
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
                            start_date: None,
                            due_date: None,
                        },
                    )
                    .unwrap();
                    // Mirrors `dispatch_item`'s own label swap: `dispatched`
                    // on, `ready-for-work` never added (removed at dispatch
                    // time in the real flow).
                    agentflare_backend::item::add_label(conn, &item.id, dispatched_id).unwrap();
                    item.id
                })
                .unwrap();

            // Alternate reasons so no 3 consecutive cycles are identical --
            // the identical-reason cap must never trip here, only the
            // any-reason one.
            let any_cap = crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP_ANY_REASON;
            for i in 0..any_cap {
                let reason = if i % 2 == 0 { "error A" } else { "error B" };
                seed_dispatch_cycle_failures(&mcp, &item_id, 1, reason);
                std::thread::sleep(std::time::Duration::from_secs(1));
            }

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

            // Daemon dies mid-job: the row is left `running`, one more
            // orphan-restart cycle beyond the seeded history.
            queue.dequeue().unwrap();

            reconcile_orphaned_jobs(&queue);

            let labels = mcp
                .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id))
                .unwrap()
                .unwrap();
            assert!(
                labels.contains(&label_ids[crate::supervisor::NEEDS_MANUAL_LABEL]),
                "the any-reason cap must land on needs-manual-dispatch, same as the \
                 identical-reason cap does"
            );
            assert!(
                !labels.contains(&label_ids[crate::supervisor::READY_LABEL]),
                "at the any-reason cap, ready-for-work would just retry-loop forever (item #164)"
            );
            assert!(
                !labels.contains(dispatched_id),
                "the stale dispatched label must still be removed"
            );
            let comments = mcp
                .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id))
                .unwrap()
                .unwrap();
            assert!(
                comments.iter().any(|c| c
                    .body
                    .starts_with(crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP_MARKER)),
                "the any-reason cap trip must post a supervisor comment for PM review, just \
                 like the identical-reason cap does"
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
                            start_date: None,
                            due_date: None,
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
                    start_date: None,
                    due_date: None,
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

    fn seed_dispatch_cycle_failures(
        mcp: &crate::mcp_server::AgentflareMcp,
        item_id: &str,
        cycles: u32,
        reason: &str,
    ) {
        let failure_body = format!(
            "{}\n\n{reason}",
            crate::dispatch_failure_ceiling::WORK_FAILURE_MARKER
        );
        mcp.with_backend_db(|conn| {
            for i in 0..cycles {
                agentflare_backend::comment::create(
                    conn,
                    item_id,
                    "test",
                    &format!(
                        "{}\n\njob: cycle-{i}",
                        crate::dispatch_failure_ceiling::DISPATCH_MARKER
                    ),
                )
                .unwrap();
                // Second-resolution timestamps + random nanoid ids can reorder
                // comments posted in the same second — stagger so dispatch
                // always precedes its failure in `list_by_item` order.
                std::thread::sleep(std::time::Duration::from_secs(1));
                agentflare_backend::comment::create(conn, item_id, "test", &failure_body).unwrap();
                if i + 1 < cycles {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
            }
            Some(())
        })
        .unwrap();
    }

    fn wait_for_terminal_job(queue: &Queue, id: &str) -> agentflare_jobs::JobInfo {
        for _ in 0..500 {
            if let Ok(info) = queue.get(id)
                && info.state.is_terminal()
            {
                return info;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("job {id} did not reach terminal state");
    }

    /// Mirrors `WorkItemExecutor`'s per-attempt failure path: each queue
    /// retry calls `release_and_comment` before returning a retryable error.
    struct RetryAwareFailingExecutor {
        mcp: std::sync::Arc<crate::mcp_server::AgentflareMcp>,
        reason: String,
    }

    impl agentflare_jobs::InProcessExecutor for RetryAwareFailingExecutor {
        fn execute(
            &self,
            job_id: &str,
            args: &[String],
            _log: &mut dyn std::io::Write,
        ) -> Result<(), agentflare_jobs::JobFailure> {
            let Some(item_id) = args.first() else {
                return Err("malformed in-process work job: missing item_id".into());
            };
            let agent = args.get(1).map(String::as_str).unwrap_or("claude-code");
            let owner = format!("{agent}:{job_id}");
            crate::claims::with_owner_override(owner, || {
                crate::cli::work::release_and_comment(&self.mcp, item_id, &self.reason, None);
            });
            Err(agentflare_jobs::JobFailure {
                message: self.reason.clone(),
                retry_after_secs: None,
                fatal: false,
            })
        }
    }

    /// Item #463/#506: a job that runs to completion and cleanly fails after
    /// exhausting `max_retries` is *not* orphaned (the process never died),
    /// so `reconcile_orphaned_jobs` above never sees it. Without this hook
    /// the item stays labeled `dispatched` forever -- invisible to
    /// `run_discovery_tick`, silently undispatchable. After
    /// `DISPATCH_FAILURE_CAP` consecutive identical dispatch cycles, lands on
    /// `needs-manual-dispatch` when that label exists.
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
            seed_dispatch_cycle_failures(
                &mcp,
                &item_id,
                crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP,
                "judge reply was not valid JSON",
            );

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
                "identical-failure cap must land on needs-manual-dispatch (item #506)"
            );
            assert!(
                !labels.contains(dispatched_id),
                "the stale dispatched label must be removed"
            );
            assert!(
                !labels.contains(&label_ids[crate::supervisor::READY_LABEL]),
                "at the identical-failure cap, ready-for-work would just retry-loop"
            );
            let comments = mcp
                .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id))
                .unwrap()
                .unwrap();
            assert!(
                comments.iter().any(|c| c
                    .body
                    .starts_with(crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP_MARKER)),
                "cap trip must post a supervisor comment for PM review"
            );
        });
    }

    /// Item #506 CodeRabbit follow-up: at cap, `assignee_agent` must be
    /// restored too (same as the below-cap branch) so the cap comment's own
    /// `item action=redispatch` instruction works without the caller having
    /// to pass `assignee_agent` explicitly.
    #[test]
    fn handle_terminal_job_failure_restores_assignee_agent_at_cap() {
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
            seed_dispatch_cycle_failures(
                &mcp,
                &item_id,
                crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP,
                "judge reply was not valid JSON",
            );

            let job = agentflare_jobs::AgentJob::new("agentflare-work")
                .args([
                    item_id.clone(),
                    "claude-code".to_string(),
                    repo_root.to_string_lossy().to_string(),
                ])
                .in_process();

            handle_terminal_job_failure(&job);

            let item = mcp
                .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id))
                .unwrap()
                .unwrap();
            assert_eq!(
                item.assignee_agent.as_deref(),
                Some("claude-code"),
                "at-cap terminal hook must restore assignee_agent so \
                 `item action=redispatch` works without an explicit override"
            );
        });
    }

    /// Below the cap, a project with `needs-manual-dispatch` still gets
    /// `ready-for-work` so a transient failure can auto-redispatch.
    #[test]
    fn handle_terminal_job_failure_restores_ready_for_work_below_identical_failure_cap() {
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
            seed_dispatch_cycle_failures(&mcp, &item_id, 1, "transient network blip");

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
            assert!(labels.contains(&label_ids[crate::supervisor::READY_LABEL]));
            assert!(!labels.contains(&label_ids[crate::supervisor::NEEDS_MANUAL_LABEL]));
        });
    }

    /// Same clean-failure path as above, but the project never created a
    /// `needs-manual-dispatch` label (that label is only ever added by hand
    /// -- unlike `ready-for-work`/`dispatched`, nothing seeds it). Below the
    /// identical-failure cap, falling back to `ready-for-work` keeps the
    /// item dispatchable.
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

    /// At the identical-failure cap without a `needs-manual-dispatch` label,
    /// the item must not go back to `ready-for-work` (that was the unbounded
    /// retry-loop path before item #506).
    #[test]
    fn handle_terminal_job_failure_stops_auto_redispatch_at_cap_without_needs_manual_label() {
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
            seed_dispatch_cycle_failures(
                &mcp,
                &item_id,
                crate::dispatch_failure_ceiling::DISPATCH_FAILURE_CAP,
                "same deterministic error",
            );

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
                !labels.contains(&label_ids[crate::supervisor::READY_LABEL]),
                "at cap the item must not be auto-redispatchable"
            );
            assert!(!labels.contains(dispatched_id));
        });
    }

    /// Item #506: default `max_retries = 3` posts four identical failure
    /// comments within one dispatch cycle. The cap must count that as one
    /// cycle — first terminal hook restores `ready-for-work`, not
    /// `needs-manual-dispatch`.
    #[test]
    fn handle_terminal_job_failure_after_default_intra_job_retries_still_auto_redispatches() {
        crate::paths::test_support::with_temp_home(|| {
            let tmp = tempfile::tempdir().unwrap();
            let repo_root = tmp.path().join("repo");
            std::fs::create_dir_all(&repo_root).unwrap();
            init_test_repo(&repo_root);

            let mcp = std::sync::Arc::new(crate::mcp_server::AgentflareMcp::for_project_dir(
                repo_root.clone(),
            ));
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

            let reason = "judge reply was not valid JSON";
            mcp.with_backend_db(|conn| {
                agentflare_backend::comment::create(
                    conn,
                    &item_id,
                    "test",
                    &format!(
                        "{}\n\njob: integration-test",
                        crate::dispatch_failure_ceiling::DISPATCH_MARKER
                    ),
                )
                .unwrap();
                Some(())
            })
            .unwrap();
            // Second-resolution timestamps can reorder comments posted in the
            // same second — ensure dispatch precedes failure comments in
            // `list_by_item` order (same stagger as `seed_dispatch_cycle_failures`).
            std::thread::sleep(std::time::Duration::from_secs(1));

            let queue = test_queue();
            let job = agentflare_jobs::AgentJob::new("agentflare-work")
                .args([
                    item_id.clone(),
                    "claude-code".to_string(),
                    repo_root.to_string_lossy().to_string(),
                ])
                .in_process();
            let info = queue.enqueue(&job).unwrap();

            let mut pool = agentflare_jobs::WorkerPool::new(queue.clone())
                .with_executor(std::sync::Arc::new(RetryAwareFailingExecutor {
                    mcp: mcp.clone(),
                    reason: reason.into(),
                }))
                .with_terminal_failure_hook(std::sync::Arc::new(|_job_id, job| {
                    handle_terminal_job_failure(job);
                }));
            pool.start(1);

            let terminal = wait_for_terminal_job(&queue, &info.id);
            pool.shutdown();

            assert_eq!(terminal.state, JobState::Failed);
            assert_eq!(
                terminal.retries, 3,
                "default max_retries budget must be exhausted before the hook runs"
            );

            let comments = mcp
                .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id))
                .unwrap()
                .unwrap();
            let failure_comments = comments
                .iter()
                .filter(|c| {
                    c.body
                        .starts_with(crate::dispatch_failure_ceiling::WORK_FAILURE_MARKER)
                })
                .count();
            assert_eq!(
                failure_comments, 4,
                "one dispatch cycle with max_retries=3 posts four failure comments"
            );
            assert_eq!(
                crate::dispatch_failure_ceiling::consecutive_identical_failure_count(&comments),
                1,
                "intra-job retries must not inflate the dispatch-cycle count"
            );

            let labels = mcp
                .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id))
                .unwrap()
                .unwrap();
            assert!(
                labels.contains(&label_ids[crate::supervisor::READY_LABEL]),
                "first dispatch-cycle terminal failure must restore ready-for-work"
            );
            assert!(
                !labels.contains(&label_ids[crate::supervisor::NEEDS_MANUAL_LABEL]),
                "intra-job retries must not trip the cap on the first dispatch cycle"
            );
            assert!(!labels.contains(dispatched_id));
        });
    }

    /// Item #506: below the cap, `release_and_comment` clears `assignee_agent`
    /// before the terminal hook runs. Without restoring it, the next discovery
    /// tick hits `skip_item` even though `ready-for-work` was restored.
    #[test]
    fn handle_terminal_job_failure_leaves_item_dispatchable_by_discovery_tick() {
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
            let item_id = mcp
                .with_backend_db(|conn| {
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
                            name: "terminal failure discovery tick test item".into(),
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
                            start_date: None,
                            due_date: None,
                        },
                    )
                    .unwrap();
                    agentflare_backend::item::add_label(conn, &item.id, dispatched_id).unwrap();
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
                crate::cli::work::release_and_comment(
                    &mcp,
                    &item_id,
                    "transient network blip",
                    None,
                );
            });

            handle_terminal_job_failure(&job);

            let item = mcp
                .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id))
                .unwrap()
                .unwrap();
            assert_eq!(
                item.assignee_agent.as_deref(),
                Some("claude-code"),
                "below-cap terminal hook must restore assignee_agent after release_and_comment (item #150)"
            );
            let labels = mcp
                .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id))
                .unwrap()
                .unwrap();
            assert!(labels.contains(&label_ids[crate::supervisor::READY_LABEL]));
            assert!(!labels.contains(dispatched_id));

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
                "below-cap terminal failure must auto-redispatch on the next discovery tick (item #506)"
            );
            assert_eq!(result.skipped, 0);
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
