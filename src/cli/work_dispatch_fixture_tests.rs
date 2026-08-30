// Shared `execute_work_impl` dispatch fixture + its two direct tests. Split
// out of `work.rs` to keep that file under the LOC gate. Included from
// `work::tests` via `include!`.

/// Mock `SendMessage` for `sdd_loop`-driven tests below: answers the judge's prompt ("You are
/// the judge") with a `complete_pipeline` decision, everything else with a plain role reply.
const JUDGE_COMPLETE_DECISION: &str = r#"{"action":"complete_pipeline","rationale":"done","ledger_line":"Task 0: complete","task_model_tier":null}"#;
fn mock_sdd_send() -> flare_workflow::json::SendMessage {
    std::sync::Arc::new(move |inv: flare_workflow::json::StepInvocation| {
        let p = inv.prompt;
        Box::pin(async move {
            if p.contains("You are the judge") {
                Ok((JUDGE_COMPLETE_DECISION.to_string(), 1u64, 0u64))
            } else {
                Ok(("DONE: did the work".to_string(), 1u64, 0u64))
            }
        })
    })
}

/// Shared setup for the two `execute_work_impl` dispatch tests below,
/// which differ only in what they assert afterward: seeds a project +
/// item under `repo_root`, dispatches it through `execute_work_impl`
/// with a mocked `sdd_loop` pipeline (`mock_sdd_send`), and returns the
/// seeding `AgentflareMcp` + item + outcome for the caller to inspect.
/// Must run inside `crate::paths::test_support::with_temp_home` (see
/// callers) -- `AgentflareMcp::for_project_dir` only overrides the
/// project-link/worktree axes, not `backend_db`, which resolves via
/// `crate::paths::home()`.
fn run_dispatch_fixture(
    repo_root: &std::path::Path,
) -> (AgentflareMcp, agentflare_backend::item::Item, WorkOutcome) {
    let seed_mcp = AgentflareMcp::for_project_dir(repo_root.to_path_buf());
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
        repo_root: Some(repo_root.to_path_buf()),
    };
    let mut log = Vec::new();
    let outcome = execute_work_impl(
        work_args,
        &mut log,
        |mcp,
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
            // Something real to commit -- otherwise `finalize`'s
            // `item_done` sees a never-diverged branch and treats the
            // run as a no-op instead of a completion. `worktree_path` is
            // this item's actual worktree; the pipeline no longer chdirs
            // the process into it (item #205), so write there explicitly.
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
        },
    );
    (seed_mcp, item, outcome)
}

#[test]
fn execute_work_runs_through_the_pipeline_but_hard_errors_without_a_github_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_root).unwrap();
    // A local bare "origin" so `git push` itself succeeds -- same
    // fixture shape as item_pr_failure_tests.rs. It's still not a
    // GitHub remote, so `push_and_open_pr` can't resolve a repo to
    // open a PR against; that's the known, deliberately-tested
    // soft-fail path (item #109 / PR #482), not this test's concern.
    init_test_repo_with_origin(&repo_root);

    crate::paths::test_support::with_temp_home(|| {
        let (seed_mcp, item, outcome) = run_dispatch_fixture(&repo_root);
        // The pipeline itself (coder -> review -> finalize) ran through
        // successfully and a real commit landed -- but `origin` here is
        // a local bare repo, not a real GitHub remote, so finalize's
        // push/PR step correctly soft-fails to open a PR and reports a
        // hard error (item #109 / PR #482) rather than false-completing
        // a claim whose work was never actually published.
        assert_eq!(outcome.exit_code, 1);

        let comments = seed_mcp
            .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item.id))
            .unwrap()
            .unwrap();
        assert!(
            comments
                .iter()
                .any(|c| c.body.contains("PR creation failed")),
            "expected a PR-creation-failed comment, got: {comments:?}"
        );
    });
}

include!("work_cwd_race_tests.rs");

/// Task 8: `execute_work_impl` dispatches through `run_or_resume`, which
/// persists `workflow_run_id` onto the item's metadata before polling
/// for completion (see `work_item_pipeline::persist_run_id`) — exercises
/// that persistence through the real `execute_work_impl` call site with
/// the new `item_description`/`plan_doc` params. `persist_run_id` runs
/// at dispatch time, well before `finalize`'s push/PR step, so the
/// metadata write survives even though this fixture's `origin` (a local
/// bare repo, not a real GitHub remote) makes `finalize` hard-error the
/// same way the sibling test above does (item #109 / PR #482).
#[test]
fn execute_work_persists_workflow_run_id_on_dispatch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_root).unwrap();
    init_test_repo_with_origin(&repo_root);

    crate::paths::test_support::with_temp_home(|| {
        let (seed_mcp, item, outcome) = run_dispatch_fixture(&repo_root);
        assert_eq!(outcome.exit_code, 1);

        let updated_item = seed_mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item.id).ok())
            .unwrap()
            .unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&updated_item.metadata).unwrap();
        assert!(metadata.get("workflow_run_id").is_some());
    });
}
