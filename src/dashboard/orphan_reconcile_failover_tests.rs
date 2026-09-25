// Terminal-job hook vs. agent failover and stop-on-request outcomes.
// Included from `orphan_reconcile.rs`'s tests module.

fn hook_fixture(
    outcome_comment: &str,
    assignee: Option<&str>,
) -> (
    Vec<String>,
    Option<String>,
    std::collections::HashMap<String, String>,
) {
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
    let item_id = create_dispatched_item(&mcp, &label_ids[crate::supervisor::DISPATCHED_LABEL]);
    // Two earlier identical failures: one more would trip the cap.
    seed_dispatch_cycle_failures(&mcp, &item_id, 2, "same bug");
    std::thread::sleep(std::time::Duration::from_millis(1100));
    mcp.with_backend_db(|conn| {
        agentflare_backend::comment::create(
            conn,
            &item_id,
            "test",
            &format!(
                "{}\n\njob: cycle-x",
                crate::dispatch_failure_ceiling::DISPATCH_MARKER
            ),
        )
        .unwrap();
        if let Some(agent) = assignee {
            agentflare_backend::item::update(
                conn,
                &item_id,
                agentflare_backend::item::UpdateItem {
                    assignee_agent: Some(agent.to_string()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
    })
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    mcp.with_backend_db(|conn| {
        agentflare_backend::comment::create(conn, &item_id, "test", outcome_comment).unwrap();
    })
    .unwrap();
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
    let assignee = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id))
        .unwrap()
        .unwrap()
        .assignee_agent;
    (labels, assignee, label_ids)
}

/// A failover cycle is neutral: even one identical failure short of the cap,
/// the item goes back on `ready-for-work` under its new agent.
#[test]
fn handle_terminal_job_failure_redispatches_a_failed_over_item_to_its_new_agent() {
    crate::paths::test_support::with_temp_home(|| {
        let body = format!(
            "{}\n\nmoved from claude-code to codex: out of credit",
            crate::dispatch_failure_ceiling::AGENT_FAILOVER_MARKER
        );
        let (labels, assignee, ids) = hook_fixture(&body, Some("codex"));
        assert!(labels.contains(&ids[crate::supervisor::READY_LABEL]));
        assert!(!labels.contains(&ids[crate::supervisor::DISPATCHED_LABEL]));
        assert!(!labels.contains(&ids[crate::supervisor::NEEDS_MANUAL_LABEL]));
        assert_eq!(assignee.as_deref(), Some("codex"), "sticky: not moved back");
    });
}

/// A cancelled/paused run must not be auto-redispatched.
#[test]
fn handle_terminal_job_failure_leaves_a_stopped_item_off_ready_for_work() {
    crate::paths::test_support::with_temp_home(|| {
        let body = format!(
            "{}\n\nworkflow run cancelled on request -- not retrying.",
            crate::dispatch_failure_ceiling::STOPPED_ON_REQUEST_MARKER
        );
        let (labels, _, ids) = hook_fixture(&body, None);
        assert!(!labels.contains(&ids[crate::supervisor::READY_LABEL]));
        assert!(!labels.contains(&ids[crate::supervisor::DISPATCHED_LABEL]));
        assert!(!labels.contains(&ids[crate::supervisor::NEEDS_MANUAL_LABEL]));
    });
}
