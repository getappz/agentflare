// End-to-end coverage for worktree provisioning failures during `execute_work`.
// Split out of `work.rs` to keep that file under the LOC gate.
// Included from `work::tests` via `include!`.

/// Drives `execute_work_impl` end-to-end through a real claim against a
/// `repo_root` that is deliberately NOT a git repo, so `item::claim`'s
/// `git worktree add` fails server-side and the response carries
/// `worktree_error` but no `worktree_path` -- the same server-side shape
/// `item_claim_response_includes_worktree_error_instead_of_silently_omitting_it`
/// (`mcp_server::tests::action_tests`) covers. Guards that
/// `missing_worktree_message` (unit-tested in `work_missing_worktree`) is
/// actually wired into this call site's `release_and_comment`, not just
/// defined and unused.
#[test]
fn execute_work_impl_posts_server_worktree_error_when_provisioning_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_root).unwrap();

    crate::paths::test_support::with_temp_home(|| {
        let seed_mcp = AgentflareMcp::for_project_dir(repo_root.clone());
        let item = seed_mcp
            .with_backend_db(|conn| seeded_item(&seed_mcp, conn))
            .unwrap();
        let work_args = WorkArgs {
            target: item.id.clone(),
            agent: Some(agent_registry::Agent::ClaudeCode.as_str().to_string()),
            timeout: DEFAULT_TIMEOUT_SECS,
            idle_timeout: DEFAULT_IDLE_TIMEOUT_SECS,
            max_turns: None,
            max_cost_usd: None,
            model: None,
            notify: None,
            repo_root: Some(repo_root.clone()),
        };
        let mut log = Vec::new();
        let outcome = execute_work_impl(work_args, &mut log, |_, _, _, _, _, _, _, _, _, _| {
            panic!("pipeline must not run when worktree provisioning failed");
        });

        assert_eq!(outcome.exit_code, 1);
        assert!(
            outcome.fatal,
            "worktree provisioning failure is structural, not retryable"
        );

        let comments = seed_mcp
            .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item.id))
            .unwrap()
            .unwrap();
        assert_eq!(comments.len(), 1);
        assert!(
            !comments[0].body.ends_with("(bad git state?)"),
            "comment must include the server's worktree_error, got: {}",
            comments[0].body
        );
    });
}
