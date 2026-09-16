// Regression coverage for `run_in_worktree` (`work_cwd_lock.rs`).
// Split out of `work.rs` to keep that file under the LOC gate. Included from
// `work::tests` via `include!`.

/// Regression test for item #205: `execute_work_impl` dispatches multiple
/// items concurrently by design (`work_max_concurrency`), and
/// `run_in_worktree` no longer chdirs the process to serialize that --
/// every downstream git/agent spawn takes its worktree path explicitly
/// instead. This test dispatches two separate items (separate
/// repos/worktrees) on two threads at once, sleeping mid-run to widen any
/// race window, and asserts: each pipeline only ever sees its own item's
/// worktree path (never the sibling's), and the process's own cwd never
/// moves throughout -- the concurrency-safety property the old chdir+mutex
/// used to provide a different, more expensive way.
#[test]
fn execute_work_impl_never_mixes_up_worktrees_under_concurrent_dispatch() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    let repo_a = tmp_a.path().join("repo");
    let repo_b = tmp_b.path().join("repo");
    std::fs::create_dir_all(&repo_a).unwrap();
    std::fs::create_dir_all(&repo_b).unwrap();
    init_test_repo_with_origin(&repo_a);
    init_test_repo_with_origin(&repo_b);

    let cwd_before_dispatch = std::env::current_dir().unwrap();

    crate::paths::test_support::with_temp_home(|| {
        let mcp_a = AgentflareMcp::for_project_dir(repo_a.clone());
        let item_a = mcp_a
            .with_backend_db(|conn| seeded_item(&mcp_a, conn))
            .unwrap();
        let mcp_b = AgentflareMcp::for_project_dir(repo_b.clone());
        let item_b = mcp_b
            .with_backend_db(|conn| seeded_item(&mcp_b, conn))
            .unwrap();

        // Same shape as `execute_work_impl`'s own `run_pipeline` parameter,
        // which this closure feeds into -- not something to alias away here.
        #[allow(clippy::type_complexity)]
        fn make_pipeline(
            expected_root: std::path::PathBuf,
        ) -> impl FnOnce(
            std::sync::Arc<AgentflareMcp>,
            &agentflare_backend::item::Item,
            &std::path::Path,
            agent_registry::Agent,
            agent_registry::Agent,
            String,
            Option<String>,
            Option<String>,
            Duration,
            Duration,
            Vec<String>,
        ) -> Result<(), String> {
            move |mcp,
                  item,
                  worktree_path,
                  implementer_agent,
                  review_agent,
                  item_description,
                  plan_doc,
                  notify,
                  timeout,
                  idle_timeout,
                  extra_args| {
                assert!(
                    worktree_path.starts_with(&expected_root),
                    "pipeline received a different item's worktree path: {worktree_path:?} \
                     not under {expected_root:?}"
                );
                // Widen the race window against the sibling thread -- with
                // no shared process-wide state left to corrupt, this is now
                // just a liveness check that both dispatches genuinely
                // overlap rather than proving anything by itself.
                std::thread::sleep(Duration::from_millis(150));
                std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();
                let _ = (timeout, idle_timeout, extra_args);
                crate::work_item_pipeline::run_or_resume_with_sender(
                    mcp,
                    item,
                    worktree_path,
                    implementer_agent,
                    review_agent,
                    item_description,
                    plan_doc,
                    notify,
                    mock_sdd_send(),
                )
            }
        }

        let args_a = WorkArgs {
            target: item_a.id.clone(),
            agent: Some(agent_registry::Agent::ClaudeCode.as_str().to_string()),
            timeout: DEFAULT_TIMEOUT_SECS,
            idle_timeout: DEFAULT_IDLE_TIMEOUT_SECS,
            max_turns: None,
            max_cost_usd: None,
            model: None,
            notify: None,
            repo_root: Some(repo_a.clone()),
        };
        let args_b = WorkArgs {
            target: item_b.id.clone(),
            agent: Some(agent_registry::Agent::ClaudeCode.as_str().to_string()),
            timeout: DEFAULT_TIMEOUT_SECS,
            idle_timeout: DEFAULT_IDLE_TIMEOUT_SECS,
            max_turns: None,
            max_cost_usd: None,
            model: None,
            notify: None,
            repo_root: Some(repo_b.clone()),
        };

        let pipeline_a = make_pipeline(repo_a.clone());
        let pipeline_b = make_pipeline(repo_b.clone());

        let handle_a =
            std::thread::spawn(move || execute_work_impl(args_a, &mut Vec::new(), pipeline_a));
        let handle_b =
            std::thread::spawn(move || execute_work_impl(args_b, &mut Vec::new(), pipeline_b));

        // What this test actually checks is that neither thread ever saw
        // the other's worktree path, asserted inside `make_pipeline`'s
        // closure -- the dispatch outcome itself (push/PR against a fake
        // local "origin") is a different concern, covered by the sibling
        // fixture tests, so it's deliberately not re-asserted here.
        handle_a.join().expect("thread A panicked -- see assertion above");
        handle_b.join().expect("thread B panicked -- see assertion above");
    });

    assert_eq!(
        std::env::current_dir().unwrap(),
        cwd_before_dispatch,
        "concurrent dispatch mutated the process's own cwd"
    );
}
