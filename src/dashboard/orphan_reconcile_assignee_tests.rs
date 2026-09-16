// Item #230: `restore_ready_for_work` and `handle_terminal_job_failure` must
// restore the item's own *current* `assignee_agent`, not the dead job's
// frozen payload agent -- a manual reassignment made after the job was
// enqueued (e.g. away from a rate-limited agent) must survive reconciliation
// instead of silently reverting to whoever the stale job was originally
// dispatched to. Split out of `orphan_reconcile_tests.rs` to keep that file
// under the project's LOC gate.

#[test]
fn restore_ready_for_work_preserves_a_reassignment_made_after_the_job_was_enqueued() {
    crate::paths::test_support::with_temp_home(|| {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        init_test_repo(&repo_root);

        let mcp = crate::mcp_server::AgentflareMcp::for_project_dir(repo_root.clone());
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
                        name: "reassignment survives orphan reconcile test item".into(),
                        description: Some("do the thing".into()),
                        priority: None,
                        parent_id: None,
                        // Manually reassigned (e.g. `item action=update`)
                        // after the now-dead job for "claude-code" was
                        // enqueued -- the scenario the frozen payload
                        // agent must not clobber.
                        assignee_agent: Some("opencode".into()),
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
                item.id
            })
            .unwrap();

        // Dead job's own frozen payload agent -- stale, no longer the
        // item's current assignee.
        restore_ready_for_work(&mcp, &item_id, "claude-code", "a-dead-job-id");

        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id))
            .unwrap()
            .unwrap();
        assert_eq!(
            item.assignee_agent.as_deref(),
            Some("opencode"),
            "a manual reassignment after the dead job was enqueued must survive orphan \
             reconciliation, not get clobbered back to the frozen job payload's agent (item #230)"
        );
    });
}

#[test]
fn handle_terminal_job_failure_preserves_a_reassignment_made_after_the_job_was_enqueued() {
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
                        name: "reassignment survives terminal failure hook test item".into(),
                        description: Some("do the thing".into()),
                        priority: None,
                        parent_id: None,
                        // Manually reassigned after the now-dead
                        // "claude-code" job was enqueued.
                        assignee_agent: Some("opencode".into()),
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

        // Dead job's own frozen payload agent -- stale, no longer the
        // item's current assignee.
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
            Some("opencode"),
            "a manual reassignment after the dead job was enqueued must survive the \
             terminal-failure hook, not get clobbered back to the frozen job payload's \
             agent (item #230)"
        );
    });
}
