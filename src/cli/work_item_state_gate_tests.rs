// Item #226: a job queued (or queued-for-retry) against an item that's
// since been cancelled must not reach `item_claim` and reopen it, even
// though the queued job's payload still carries the item's original
// assignee (which is exactly what lets it slip past `claim()`'s
// `BlockedByAssignee` freeze). The execution-time state re-check added
// alongside `fresh_dispatch_pair`'s execution-time agent/model
// re-resolve must catch this before any claim/dispatch happens.
#[test]
fn work_item_executor_refuses_a_stale_job_against_a_cancelled_item() {
    use agentflare_jobs::InProcessExecutor;

    crate::paths::test_support::with_temp_home(|| {
        let tmp = tempfile::tempdir().unwrap();
        let repo_root = tmp.path().join("repo");
        std::fs::create_dir_all(&repo_root).unwrap();
        init_test_repo(&repo_root);
        let mcp = AgentflareMcp::for_project_dir(repo_root.clone());
        let item = mcp.with_backend_db(|conn| seeded_item(&mcp, conn)).unwrap();
        mcp.with_backend_db(|conn| {
            let cancelled =
                agentflare_backend::state::first_in_group(conn, &item.project_id, "cancelled")
                    .unwrap();
            agentflare_backend::item::update_state(conn, &item.id, &cancelled.id).unwrap();
        })
        .unwrap();

        let mut log: Vec<u8> = Vec::new();
        let result = WorkItemExecutor.execute(
            "job-1",
            &[
                item.id.clone(),
                "claude-code".into(),
                repo_root.to_string_lossy().into_owned(),
            ],
            &mut log,
        );

        let failure = result.expect_err("a cancelled item's stale job must be refused");
        assert!(
            failure.fatal,
            "must not consume the retry budget re-checking an item that can't un-cancel itself"
        );
        assert!(failure.message.contains("cancelled"));

        let refreshed = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item.id))
            .unwrap()
            .unwrap();
        let state = mcp
            .with_backend_db(|conn| agentflare_backend::state::get(conn, &refreshed.state_id))
            .unwrap()
            .unwrap();
        assert_eq!(
            state.group_name, "cancelled",
            "the stale job must not reopen the cancelled item"
        );
    });
}
