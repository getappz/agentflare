// Regression coverage for the `EXECUTE_WORK_CWD_LOCK` fix in `execute_work_impl`.
// Split out of `work.rs` to keep that file under the LOC gate. Included from
// `work::tests` via `include!`.

/// Regression test for the `set_current_dir` race `EXECUTE_WORK_CWD_LOCK`
/// fixes: the daemon dispatches multiple items' `execute_work_impl` calls
/// concurrently by design (`work_max_concurrency`), and without the lock
/// two of them chdir-ing around the same time can have one item's
/// pipeline run against a *different* item's worktree -- observed live,
/// twice, with two different item pairs. This test dispatches two
/// separate items (separate repos/worktrees) on two threads at once; each
/// pipeline sleeps mid-run and re-checks its cwd, so it would fail (the
/// mid-sleep `assert_eq!` below) if the lock were removed and the other
/// thread's chdir landed in between.
#[test]
fn execute_work_impl_serializes_the_cwd_dependent_section_across_concurrent_dispatch() {
    let tmp_a = tempfile::tempdir().unwrap();
    let tmp_b = tempfile::tempdir().unwrap();
    let repo_a = tmp_a.path().join("repo");
    let repo_b = tmp_b.path().join("repo");
    std::fs::create_dir_all(&repo_a).unwrap();
    std::fs::create_dir_all(&repo_b).unwrap();
    init_test_repo_with_origin(&repo_a);
    init_test_repo_with_origin(&repo_b);

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
                  implementer_agent,
                  review_agent,
                  item_description,
                  plan_doc,
                  notify,
                  timeout,
                  idle_timeout,
                  extra_args| {
                let cwd_before = std::env::current_dir().unwrap();
                assert!(
                    cwd_before.starts_with(&expected_root),
                    "chdir landed outside this thread's own repo: {cwd_before:?} \
                     not under {expected_root:?}"
                );
                // Widen the race window: without the lock, the other
                // thread's own chdir is free to land here.
                std::thread::sleep(Duration::from_millis(150));
                let cwd_after = std::env::current_dir().unwrap();
                assert_eq!(
                    cwd_before, cwd_after,
                    "cwd changed under us mid-run -- another thread's dispatch raced this one"
                );
                std::fs::write(cwd_after.join("real_work.txt"), "real work").unwrap();
                let _ = (timeout, idle_timeout, extra_args);
                crate::work_item_pipeline::run_or_resume_with_sender(
                    mcp,
                    item,
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

        let outcome_a = handle_a
            .join()
            .expect("thread A panicked -- see assertion above");
        let outcome_b = handle_b
            .join()
            .expect("thread B panicked -- see assertion above");

        // Same soft-fail-on-no-GitHub-remote outcome as the sibling
        // dispatch test above -- what this test actually checks is that
        // neither thread's cwd ever drifted into the other's worktree,
        // asserted inside `make_pipeline`'s closure.
        assert_eq!(outcome_a.exit_code, 1);
        assert_eq!(outcome_b.exit_code, 1);
    });
}
