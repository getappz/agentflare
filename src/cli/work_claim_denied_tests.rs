// Regression coverage for #204: pre-pipeline early returns in
// `execute_work_impl` reported failures only to `crate::ui::error` (the
// daemon's own stderr for an in-process job), never to the `log` parameter
// that becomes the job's captured stdout/stderr file -- the only failure
// surface the dashboard/operators actually see. Reproduces the literal
// job `aY19fOWVYnG9pr9Ws1lUt` scenario: a claim denied because another
// owner already holds it.

#[test]
fn execute_work_impl_logs_the_reason_when_claim_is_held_by_another_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_root = tmp.path().join("repo");
    std::fs::create_dir_all(&repo_root).unwrap();
    init_test_repo(&repo_root);

    crate::paths::test_support::with_temp_home(|| {
        // Same `AgentflareMcp::for_project_dir` construction `execute_work_impl`
        // uses internally (see `for_project_dir` call a few lines into
        // `execute_work_impl`) -- a `for_test`-constructed instance with
        // explicit db/link overrides would resolve a different backend db
        // and the seeded item would be invisible to it.
        let seed_mcp = AgentflareMcp::for_project_dir(repo_root.clone());
        let item = seed_mcp
            .with_backend_db(|conn| seeded_item(&seed_mcp, conn))
            .unwrap();

        // A different owner claims it first and holds it live.
        crate::claims::with_owner_override("agent:other", || {
            let claim_json = seed_mcp
                .item_claim(ItemRequest {
                    action: "claim".to_string(),
                    id: Some(item.id.clone()),
                    ..Default::default()
                })
                .unwrap();
            let claim: serde_json::Value = serde_json::from_str(&claim_json).unwrap();
            assert_eq!(claim["status"], "acquired");
        });

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
        let outcome = execute_work_impl(work_args, &mut log, |_, _, _, _, _, _, _, _, _, _, _| {
            panic!("pipeline must not run when the claim is held by another owner");
        });

        assert_eq!(outcome.exit_code, 1);
        let log_text = String::from_utf8(log).unwrap();
        assert!(
            !log_text.is_empty(),
            "claim-denied failure must be captured in the job's own log, not just stderr \
             (#204 / job aY19fOWVYnG9pr9Ws1lUt produced a 0-byte log for exactly this case)"
        );
        assert!(
            log_text.contains("held by"),
            "log must name the actual denial reason, got: {log_text}"
        );
    });
}
