use super::*;

#[path = "supervisor_project_scope_tests.rs"]
mod project_scope_tests;
#[path = "supervisor_telegram_tests.rs"]
mod telegram_tests;

#[test]
fn resolve_confirmed_agent_accepts_claude_code() {
    assert_eq!(
        resolve_confirmed_agent("claude-code"),
        Some(agent_registry::Agent::ClaudeCode)
    );
}

#[test]
fn resolve_confirmed_agent_recognizes_an_instance_suffixed_assignee() {
    // A previously-claimed item's assignee_agent carries `<agent>:<instance>`
    // (see item::claim's doc comment) — this must still resolve, or a
    // once-claimed item can never be redispatched.
    assert_eq!(
        resolve_confirmed_agent("claude-code:some-job-id"),
        Some(agent_registry::Agent::ClaudeCode)
    );
}

#[test]
fn resolve_confirmed_agent_accepts_opencode() {
    assert_eq!(
        resolve_confirmed_agent("opencode"),
        Some(agent_registry::Agent::Opencode)
    );
}

#[test]
fn resolve_confirmed_agent_rejects_unknown_agent_string() {
    assert_eq!(resolve_confirmed_agent("not-a-real-agent"), None);
}

#[test]
fn route_unassigned_with_returns_none_when_no_rule_matches() {
    let item = agentflare_backend::item::Item {
        metadata: "{}".into(),
        ..test_item_stub()
    };
    let config = agent_registry::RouterConfig::default();
    let installed = [agent_registry::Agent::Opencode];
    let mut rotation = std::collections::HashMap::new();
    assert_eq!(
        route_unassigned_with(&item, &config, &installed, &mut rotation),
        None
    );
}

#[test]
fn route_unassigned_with_picks_the_implementer_role_rule() {
    // Item #19 regression: an item with no assignee_agent (as
    // `discover_untracked_prs` always creates) must still resolve to an
    // agent when a `[[router.rule]] when = { role = "implementer" }` rule
    // covers it, the same rule `agentflare work` itself would use.
    let item = agentflare_backend::item::Item {
        metadata: "{}".into(),
        ..test_item_stub()
    };
    let config = agent_registry::RouterConfig {
        default: None,
        rules: vec![agent_registry::RouterRule {
            when: agent_registry::RuleMatch {
                role: Some("implementer".into()),
                ..Default::default()
            },
            use_agents: vec![agent_registry::Agent::Opencode],
            rotate: false,
            model: None,
        }],
    };
    let installed = [agent_registry::Agent::Opencode];
    let mut rotation = std::collections::HashMap::new();
    assert_eq!(
        route_unassigned_with(&item, &config, &installed, &mut rotation),
        Some(agent_registry::Agent::Opencode)
    );
}

#[test]
fn route_unassigned_with_ignores_a_matching_rule_whose_agent_is_not_installed() {
    let item = agentflare_backend::item::Item {
        metadata: "{}".into(),
        ..test_item_stub()
    };
    let config = agent_registry::RouterConfig {
        default: None,
        rules: vec![agent_registry::RouterRule {
            when: agent_registry::RuleMatch {
                role: Some("implementer".into()),
                ..Default::default()
            },
            use_agents: vec![agent_registry::Agent::Opencode],
            rotate: false,
            model: None,
        }],
    };
    let installed: [agent_registry::Agent; 0] = [];
    let mut rotation = std::collections::HashMap::new();
    assert_eq!(
        route_unassigned_with(&item, &config, &installed, &mut rotation),
        None
    );
}

fn test_item_stub() -> agentflare_backend::item::Item {
    agentflare_backend::item::Item {
        id: "test-id".into(),
        project_id: "proj".into(),
        state_id: "state".into(),
        name: "test".into(),
        description: String::new(),
        priority: "none".into(),
        parent_id: None,
        assignee_agent: None,
        sequence_id: 1,
        sort_order: 0.0,
        started_at: None,
        completed_at: None,
        archived_at: None,
        external_source: None,
        external_id: None,
        metadata: "{}".into(),
        created_at: 0,
        updated_at: 0,
        deleted_at: None,
        start_date: None,
        due_date: None,
    }
}

fn test_mcp() -> AgentflareMcp {
    AgentflareMcp::for_test_memory()
}

fn test_queue() -> agentflare_jobs::Queue {
    let dir = tempfile::tempdir().unwrap().keep();
    agentflare_jobs::Queue::open_memory(dir.join("logs")).unwrap()
}

fn test_auth_conn() -> rusqlite::Connection {
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    crate::auth_db::migrate(&conn).unwrap();
    conn
}

fn seed_ready_item(mcp: &AgentflareMcp, assignee: Option<&str>) -> String {
    mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).unwrap();
        for name in ["ready-for-work", "dispatched", "needs-manual-dispatch"] {
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
        let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
        let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
        let item = agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project.id.clone(),
                state_id,
                name: "Do the thing".into(),
                description: Some("do it well".into()),
                priority: None,
                parent_id: None,
                assignee_agent: assignee.map(str::to_string),
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
        let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
        let ready_id = &labels
            .iter()
            .find(|l| l.name == "ready-for-work")
            .unwrap()
            .id;
        agentflare_backend::item::add_label(conn, &item.id, ready_id).unwrap();
        item.id
    })
    .unwrap()
}

fn labels_contain_name(mcp: &AgentflareMcp, label_ids: &[String], name: &str) -> bool {
    mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).unwrap();
        let all = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
        let target = all.iter().find(|l| l.name == name).unwrap();
        label_ids.contains(&target.id)
    })
    .unwrap()
}

#[test]
fn confirmed_agent_gets_dispatched_and_relabeled() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_ready_item(&mcp, Some("claude-code"));

    let auth_conn = test_auth_conn();
    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.dispatched, 1);
    assert_eq!(result.skipped, 0);

    let jobs = queue.list(None).unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(
        jobs[0].in_process,
        "work-item jobs must run in-process (item #19)"
    );
    assert!(jobs[0].args.contains(&item_id));
    assert!(jobs[0].args.contains(&"claude-code".to_string()));

    let labels = mcp
        .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
        .unwrap();
    assert!(!labels_contain_name(&mcp, &labels, "ready-for-work"));
    assert!(labels_contain_name(&mcp, &labels, "dispatched"));
}

#[test]
fn item_model_override_reads_metadata_model_key() {
    assert_eq!(
        item_model_override(r#"{"model": "anthropic/claude-sonnet-5"}"#),
        Some("anthropic/claude-sonnet-5".to_string())
    );
    assert_eq!(item_model_override("{}"), None);
    assert_eq!(item_model_override("not json"), None);
    assert_eq!(item_model_override(r#"{"model": 5}"#), None);
}

#[test]
fn dispatched_job_carries_a_metadata_model_override() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_ready_item(&mcp, Some("opencode"));
    mcp.with_backend_db(|conn| {
        agentflare_backend::item::update(
            conn,
            &item_id,
            agentflare_backend::item::UpdateItem {
                metadata: Some(r#"{"model": "anthropic/claude-sonnet-5"}"#.into()),
                ..Default::default()
            },
        )
    })
    .unwrap()
    .unwrap();

    let auth_conn = test_auth_conn();
    run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    let jobs = queue.list(None).unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(
        jobs[0]
            .args
            .contains(&"anthropic/claude-sonnet-5".to_string())
    );
}

#[test]
fn agent_in_cooldown_is_skipped_not_dispatched() {
    let mcp = test_mcp();
    let queue = test_queue();
    let auth_conn = test_auth_conn();
    let item_id = seed_ready_item(&mcp, Some("claude-code"));
    crate::auth_db::set_cooldown(&auth_conn, "claude-code", "__default__", 30, "rate limit");

    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.dispatched, 0);
    assert_eq!(result.skipped, 0);
    assert_eq!(
        result.waiting, 1,
        "a cooling-down item must count as waiting, not vanish silently (item #82)"
    );
    assert!(
        queue.list(None).unwrap().is_empty(),
        "a cooling-down agent must not be dispatched"
    );

    let labels = mcp
        .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
        .unwrap();
    assert!(
        labels_contain_name(&mcp, &labels, "ready-for-work"),
        "the item must stay ready-for-work so a later tick can pick it up once the cooldown clears"
    );
}

#[test]
fn needs_decision_label_blocks_dispatch_even_though_ready_for_work_is_present() {
    // The gated branch now fires a best-effort Telegram notify -- run under
    // an isolated home so this can't read (or send through) the developer's
    // real vault, same reasoning as channels.rs's own vault-touching tests.
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        let auth_conn = test_auth_conn();
        let item_id = seed_ready_item(&mcp, Some("claude-code"));
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            agentflare_backend::label::create(
                conn,
                agentflare_backend::label::CreateLabel {
                    project_id: Some(project.id.clone()),
                    workspace_id: project.workspace_id.clone(),
                    name: NEEDS_DECISION_LABEL.into(),
                    color: None,
                    parent_id: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap();
            let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
            let gate_id = &labels
                .iter()
                .find(|l| l.name == NEEDS_DECISION_LABEL)
                .unwrap()
                .id;
            agentflare_backend::item::add_label(conn, &item_id, gate_id).unwrap();
            Some(())
        })
        .unwrap();

        let result = run_discovery_tick(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(result.dispatched, 0);
        assert_eq!(result.skipped, 0);
        assert_eq!(
            result.waiting, 1,
            "a go/no-go item gated on a pending decision must count as waiting, not dispatch"
        );
        assert!(
            queue.list(None).unwrap().is_empty(),
            "a needs-decision item must never reach the job queue, regardless of ready-for-work"
        );

        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(
            labels_contain_name(&mcp, &labels, "ready-for-work"),
            "the gate must not touch ready-for-work -- redispatch re-attaches it unconditionally, \
             so needs-decision has to keep blocking on its own"
        );
        assert!(labels_contain_name(&mcp, &labels, NEEDS_DECISION_LABEL));
    });
}

#[path = "supervisor/tests/host_gate_tests.rs"]
mod host_gate_tests;

fn seed_ready_item_under_gated_goal(mcp: &AgentflareMcp) -> String {
    mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).unwrap();
        for name in [
            "ready-for-work",
            "dispatched",
            "needs-manual-dispatch",
            NEEDS_HUMAN_GATE_LABEL,
        ] {
            let _ = agentflare_backend::label::create(
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
            );
        }
        let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
        let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
        let goal_metadata = serde_json::json!({
            "goal": {
                "objective": "ship it",
                "scope": { "allowed_paths": [], "disallowed_actions": [] },
                "quota_mode": "default",
                "lifecycle": "gated",
                "consecutive_self_repairs": 0,
            }
        })
        .to_string();
        let goal_item = agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project.id.clone(),
                state_id: state_id.clone(),
                name: "goal".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: Some(goal_metadata),
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
                start_date: None,
                due_date: None,
            },
        )
        .unwrap();
        let item = agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project.id.clone(),
                state_id,
                name: "Do the thing".into(),
                description: Some("do it well".into()),
                priority: None,
                parent_id: Some(goal_item.id.clone()),
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
        let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
        let ready_id = &labels
            .iter()
            .find(|l| l.name == "ready-for-work")
            .unwrap()
            .id;
        agentflare_backend::item::add_label(conn, &item.id, ready_id).unwrap();
        item.id
    })
    .unwrap()
}

#[test]
fn gated_goal_never_dispatches_and_relabels_to_needs_human_gate() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_ready_item_under_gated_goal(&mcp);

    let auth_conn = test_auth_conn();
    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.dispatched, 0);
    assert!(
        queue.list(None).unwrap().is_empty(),
        "an ask decision must never enqueue a job"
    );

    let labels = mcp
        .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
        .unwrap();
    assert!(!labels_contain_name(&mcp, &labels, "ready-for-work"));
    assert!(labels_contain_name(&mcp, &labels, NEEDS_HUMAN_GATE_LABEL));
}

fn seed_ready_item_under_active_goal_with_repairs(
    mcp: &AgentflareMcp,
    repairs: u32,
) -> (String, String) {
    mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).unwrap();
        for name in [
            "ready-for-work",
            "dispatched",
            "needs-manual-dispatch",
            NEEDS_HUMAN_GATE_LABEL,
        ] {
            let _ = agentflare_backend::label::create(
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
            );
        }
        let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
        let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
        let goal_metadata = serde_json::json!({
            "goal": {
                "objective": "ship it",
                "scope": { "allowed_paths": [], "disallowed_actions": [] },
                "quota_mode": "default",
                "lifecycle": "active",
                "consecutive_self_repairs": repairs,
            }
        })
        .to_string();
        let goal_item = agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project.id.clone(),
                state_id: state_id.clone(),
                name: "goal".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: Some(goal_metadata),
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
                start_date: None,
                due_date: None,
            },
        )
        .unwrap();
        agentflare_backend::vent::upsert(
            conn,
            &project.id,
            "minor friction",
            "low",
            "[]",
            "topic",
            "evt-1",
            1,
            crate::claims::now(),
        )
        .unwrap();
        let vents = agentflare_backend::vent::list(conn, &project.id, false).unwrap();
        agentflare_backend::vent::set_actionable(conn, &vents[0].id, true).unwrap();
        agentflare_backend::vent::link_item(conn, &vents[0].id, &goal_item.id).unwrap();
        let item = agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project.id.clone(),
                state_id,
                name: "Do the thing".into(),
                description: Some("do it well".into()),
                priority: None,
                parent_id: Some(goal_item.id.clone()),
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
        let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
        let ready_id = &labels
            .iter()
            .find(|l| l.name == "ready-for-work")
            .unwrap()
            .id;
        agentflare_backend::item::add_label(conn, &item.id, ready_id).unwrap();
        (item.id, goal_item.id)
    })
    .unwrap()
}

#[test]
fn under_cap_self_repairs_and_dispatches() {
    let mcp = test_mcp();
    let queue = test_queue();
    let (_item_id, _goal_id) = seed_ready_item_under_active_goal_with_repairs(&mcp, 0);

    let auth_conn = test_auth_conn();
    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.dispatched, 1, "self-repair still dispatches the job");
    assert_eq!(queue.list(None).unwrap().len(), 1);
}

#[test]
fn at_cap_forces_ask_instead_of_dispatching() {
    // ask_item now fires a best-effort Telegram notify -- run under an
    // isolated home so this can't read (or send through) the developer's
    // real vault, same reasoning as channels.rs's own vault-touching tests.
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        let (item_id, _goal_id) = seed_ready_item_under_active_goal_with_repairs(
            &mcp,
            crate::quota::decide::SELF_REPAIR_CAP,
        );

        let auth_conn = test_auth_conn();
        let result = run_discovery_tick(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(
            result.dispatched, 0,
            "the cap must force ask, not another self-repair dispatch"
        );
        assert!(queue.list(None).unwrap().is_empty());
        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(labels_contain_name(&mcp, &labels, NEEDS_HUMAN_GATE_LABEL));
    });
}

#[test]
fn ungrouped_ready_item_dispatches_exactly_as_before_this_change() {
    let mcp = test_mcp();
    let queue = test_queue();
    // Reuses the pre-existing seed_ready_item helper (no goal ancestor
    // at all) — this is the plan's explicit no-regression guarantee.
    let item_id = seed_ready_item(&mcp, Some("claude-code"));

    let auth_conn = test_auth_conn();
    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.dispatched, 1);
    assert_eq!(result.skipped, 0);
    let jobs = queue.list(None).unwrap();
    assert!(jobs[0].args.contains(&item_id));
}

#[test]
fn unconfirmed_agent_gets_skipped_not_dispatched() {
    let mcp = test_mcp();
    let queue = test_queue();
    // windsurf has a REGISTRY entry but no autonomous_args mapped, so it's
    // still "unconfirmed" for autonomous dispatch (unlike opencode/cursor,
    // which both now have autonomous_args mapped).
    let item_id = seed_ready_item(&mcp, Some("windsurf"));

    let auth_conn = test_auth_conn();
    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.dispatched, 0);
    assert_eq!(result.skipped, 1);
    assert!(queue.list(None).unwrap().is_empty());

    let labels = mcp
        .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
        .unwrap();
    assert!(!labels_contain_name(&mcp, &labels, "ready-for-work"));
    assert!(labels_contain_name(&mcp, &labels, "needs-manual-dispatch"));
}

/// Seeds a ready-for-work item in a brand new project/workspace
/// (independent of whatever `test_mcp()`'s cwd-resolved project is) and
/// registers it in `project_dirs` at `folder_path` — the same registry
/// `AgentflareMcp::register_project_dir` populates for a real repo, but
/// written directly here so the test controls the folder path without
/// needing a real linked repo on disk.
fn seed_ready_item_in_project(mcp: &AgentflareMcp, name: &str, folder_path: &str) -> String {
    mcp.with_backend_db(|conn| {
        let workspace = agentflare_backend::workspace::create(
            conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: name.into(),
                slug: name.into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let project = agentflare_backend::project::create(
            conn,
            agentflare_backend::project::CreateProject {
                workspace_id: workspace.id,
                name: name.into(),
                identifier: name.into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        agentflare_backend::project_dir::upsert(conn, &project.id, folder_path, 1).unwrap();
        for label_name in ["ready-for-work", "dispatched", "needs-manual-dispatch"] {
            agentflare_backend::label::create(
                conn,
                agentflare_backend::label::CreateLabel {
                    project_id: Some(project.id.clone()),
                    workspace_id: project.workspace_id.clone(),
                    name: label_name.into(),
                    color: None,
                    parent_id: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap();
        }
        let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
        let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
        let item = agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project.id.clone(),
                state_id,
                name: "Do the thing".into(),
                description: Some("do it well".into()),
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
        let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
        let ready_id = &labels
            .iter()
            .find(|l| l.name == "ready-for-work")
            .unwrap()
            .id;
        agentflare_backend::item::add_label(conn, &item.id, ready_id).unwrap();
        item.id
    })
    .unwrap()
}

// --- run_review_sweep / self_repair_or_gate (item #65) ---

/// A throwaway git repo with no remote -- same trick
/// `item_check_merge_leaves_an_in_review_item_alone_when_merge_status_is_unknown`
/// (in `mcp_server::tests::action_tests`) uses: `RepoId::resolve_from_remote`
/// soft-fails to `None` against it, so `worktree::pr_ci_status` reports
/// `Unknown` without ever touching the network.
fn throwaway_repo() -> tempfile::TempDir {
    let repo_dir = tempfile::tempdir().unwrap();
    let run_git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(repo_dir.path())
            .output()
            .unwrap()
    };
    run_git(&["init", "-b", "master"]);
    run_git(&["config", "user.email", "test@test.com"]);
    run_git(&["config", "user.name", "Test"]);
    run_git(&["commit", "--allow-empty", "-m", "initial"]);
    repo_dir
}

fn test_mcp_with_repo(repo_root: std::path::PathBuf) -> AgentflareMcp {
    AgentflareMcp::for_test(
        repo_root.join("backend.db"),
        repo_root.clone(),
        repo_root.join("project.json"),
    )
}

/// Seeds an item already sitting in "in_review" -- claimed and moved
/// there directly via the backend calls `item_claim`/`item_done` wrap
/// (bypassing worktree creation entirely, unlike `seed_ready_item`'s
/// real-claim-through-the-MCP-method path), since these tests exercise
/// `run_review_sweep`'s decision logic, not the claim/worktree mechanics
/// already covered by `mcp_server::tests`. The claim is backdated by
/// `claim_age_secs` so tests can seed either a fresh (still-live) claim or
/// one already past its #108-capped in_review TTL (item #114).
fn seed_in_review_item_with_claim_age(
    mcp: &AgentflareMcp,
    assignee: Option<&str>,
    claim_age_secs: i64,
) -> String {
    mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).unwrap();
        let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
        let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
        let item = agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project.id.clone(),
                state_id,
                name: "Fix CI".into(),
                description: Some("do it well".into()),
                priority: None,
                parent_id: None,
                assignee_agent: assignee.map(str::to_string),
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
        let owner = assignee
            .map(|a| format!("{a}:prior-job"))
            .unwrap_or_else(|| "cli".into());
        let claimed_at = crate::claims::now() - claim_age_secs;
        agentflare_backend::item::claim(
            conn,
            &item.id,
            &owner,
            claimed_at,
            crate::claims::ttl_secs(),
        )
        .unwrap();
        agentflare_backend::item::mark_in_review(conn, &item.id, &owner).unwrap();
        item.id
    })
    .unwrap()
}

/// Same shape as `seed_ready_item_in_project` (a brand new project/workspace
/// registered in `project_dirs` at `folder_path`, independent of whatever
/// `mcp`'s own cwd-resolved project is) but seeds a fresh `in_review` item
/// instead of a `ready-for-work` one -- for pinning `run_review_sweep`'s own
/// multi-project scan (item #124), the review-sweep counterpart to
/// `seed_ready_item_in_project`'s discovery-tick coverage (item #63).
fn seed_in_review_item_in_project(mcp: &AgentflareMcp, name: &str, folder_path: &str) -> String {
    mcp.with_backend_db(|conn| {
        let workspace = agentflare_backend::workspace::create(
            conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: name.into(),
                slug: name.into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let project = agentflare_backend::project::create(
            conn,
            agentflare_backend::project::CreateProject {
                workspace_id: workspace.id,
                name: name.into(),
                identifier: name.into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        agentflare_backend::project_dir::upsert(conn, &project.id, folder_path, 1).unwrap();
        let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
        let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
        let item = agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project.id.clone(),
                state_id,
                name: "Fix CI".into(),
                description: Some("do it well".into()),
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
        let owner = "claude-code:prior-job";
        agentflare_backend::item::claim(
            conn,
            &item.id,
            owner,
            crate::claims::now(),
            crate::claims::ttl_secs(),
        )
        .unwrap();
        agentflare_backend::item::mark_in_review(conn, &item.id, owner).unwrap();
        item.id
    })
    .unwrap()
}

/// Fresh (just-claimed) `in_review` item -- the common case for tests that
/// short-circuit before item #114's claim-liveness check ever runs.
fn seed_in_review_item(mcp: &AgentflareMcp, assignee: Option<&str>) -> String {
    seed_in_review_item_with_claim_age(mcp, assignee, 0)
}

#[test]
fn run_discovery_tick_dispatches_ready_items_from_every_registered_project_not_just_one() {
    // Item #63: the daemon's own cwd-resolved project must not be the
    // only project discovery ever looks at — every project registered
    // in `project_dirs` (populated by any CLI/MCP call that ever ran
    // inside it) must get its ready-for-work items picked up too.
    let mcp = test_mcp();
    let queue = test_queue();
    let item_a = seed_ready_item_in_project(&mcp, "proj-a", "/repo/a");
    let item_b = seed_ready_item_in_project(&mcp, "proj-b", "/repo/b");

    let auth_conn = test_auth_conn();
    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(
        result.dispatched, 2,
        "both projects' ready items must be dispatched, not just one"
    );
    let jobs = queue.list(None).unwrap();
    assert_eq!(jobs.len(), 2);

    let job_a = jobs.iter().find(|j| j.args.contains(&item_a)).unwrap();
    assert!(
        job_a.args.contains(&"/repo/a".to_string()),
        "job for proj-a's item must carry proj-a's own folder path, got {:?}",
        job_a.args
    );
    let job_b = jobs.iter().find(|j| j.args.contains(&item_b)).unwrap();
    assert!(
        job_b.args.contains(&"/repo/b".to_string()),
        "job for proj-b's item must carry proj-b's own folder path, got {:?}",
        job_b.args
    );
}

fn seed_gate_label(mcp: &AgentflareMcp) -> std::collections::HashMap<String, String> {
    mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).unwrap();
        let _ = agentflare_backend::label::create(
            conn,
            agentflare_backend::label::CreateLabel {
                project_id: Some(project.id.clone()),
                workspace_id: project.workspace_id.clone(),
                name: NEEDS_HUMAN_GATE_LABEL.into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        );
        let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
        labels.into_iter().map(|l| (l.name, l.id)).collect()
    })
    .unwrap()
}

#[test]
fn job_in_flight_detects_a_queued_job_for_the_item() {
    let queue = test_queue();
    let job = agentflare_jobs::AgentJob::new("agentflare-work")
        .args(["item-1".to_string(), "claude-code".to_string()])
        .in_process();
    queue.enqueue(&job).unwrap();

    assert!(job_in_flight(&queue, "item-1"));
    assert!(!job_in_flight(&queue, "item-2"));
}

#[test]
fn second_tick_does_not_reenqueue_while_job_still_queued() {
    // Item #221: every discovery tick enqueued a fresh row per ready item
    // (96+ dupes observed live) because dispatch_item never checked for an
    // existing queued/running row. Re-arm the ready label the way a failed
    // swap or a reconcile-restore would, tick again: still exactly one row.
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_ready_item(&mcp, Some("claude-code"));

    let auth_conn = test_auth_conn();
    let first = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );
    assert_eq!(first.dispatched, 1);
    assert_eq!(queue.list(None).unwrap().len(), 1);

    mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).unwrap();
        let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
        let ready_id = labels
            .iter()
            .find(|l| l.name == "ready-for-work")
            .unwrap()
            .id
            .clone();
        agentflare_backend::item::add_label(conn, &item_id, &ready_id).unwrap();
    })
    .unwrap();

    let second = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );
    assert_eq!(second.dispatched, 0);
    assert_eq!(
        queue.list(None).unwrap().len(),
        1,
        "re-armed ready label must not produce a second queued row"
    );
}

#[test]
fn plan_gated_item_does_not_dispatch_when_blocked() {
    // Task #573: gated items (plan_required=true) with unapproved plan status
    // must not be dispatched by the supervisor -- nothing enqueued for them.
    // Unlike the `job_in_flight` skip, though, a plan gate is retryable, so it
    // must also land in `DiscoveryTickResult::waiting` rather than in no
    // counter at all (item #573 final review, Fix 2).
    let mcp = test_mcp();
    let queue = test_queue();
    let auth_conn = test_auth_conn();

    let (blocked_item_id, approved_item_id) = mcp
        .with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            // Create necessary labels
            for name in ["ready-for-work", "dispatched", "needs-manual-dispatch"] {
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
            let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
            let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();

            // Create a blocked gated item (plan_required=true, plan_status not approved)
            let blocked_item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id: state_id.clone(),
                    name: "Blocked by plan gate".into(),
                    description: None,
                    priority: None,
                    parent_id: None,
                    assignee_agent: Some("claude-code".into()),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: Some(r#"{"plan_required":true,"plan_status":"pending"}"#.into()),
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                    start_date: None,
                    due_date: None,
                },
            )
            .unwrap();

            // Create an approved gated item (plan_required=true, plan_status="approved")
            let approved_item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id: state_id.clone(),
                    name: "Approved by plan gate".into(),
                    description: None,
                    priority: None,
                    parent_id: None,
                    assignee_agent: Some("claude-code".into()),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: Some(r#"{"plan_required":true,"plan_status":"approved"}"#.into()),
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                    start_date: None,
                    due_date: None,
                },
            )
            .unwrap();

            let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
            let ready_id = &labels
                .iter()
                .find(|l| l.name == "ready-for-work")
                .unwrap()
                .id;
            agentflare_backend::item::add_label(conn, &blocked_item.id, ready_id).unwrap();
            agentflare_backend::item::add_label(conn, &approved_item.id, ready_id).unwrap();
            (blocked_item.id, approved_item.id)
        })
        .unwrap();

    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    // Only the approved item should dispatch
    assert_eq!(
        result.dispatched, 1,
        "only the approved gated item should be dispatched"
    );
    // The queue should have exactly 1 job (for the approved item, not the blocked one)
    let jobs = queue.list(None).unwrap();
    assert_eq!(jobs.len(), 1, "only the approved item should be enqueued");
    assert!(
        jobs[0].args.contains(&approved_item_id),
        "the queued job must be for the approved item"
    );
    assert!(
        !jobs[0].args.contains(&blocked_item_id),
        "the blocked item must not appear in the queue"
    );
    // Item #573 final review, Fix 2: the blocked item must be visible to an
    // operator in the tick summary. Before the fix `dispatch_item` returned a
    // bare `false` here and no counter moved at all, so an auto-gated item
    // could stall the supervisor silently and indefinitely.
    assert_eq!(
        result.waiting, 1,
        "a plan-gated item is retryable, so it must be counted as waiting"
    );
    assert_eq!(
        result.skipped, 0,
        "waiting is not skipped -- skipped reads as a decision that won't be revisited"
    );
}

/// Item #573 final review, Fix 2: an item gated with NO plan submitted yet
/// (`plan_status` absent -> `"none"`) is the stall case Task 7's default
/// policy makes common, and nothing else in the system would ever ping a
/// human about it (`item_submit_plan` sends the approve card, and it was never
/// called). It must count as `waiting` AND fire the one-time human notify.
#[test]
fn plan_gated_item_with_no_plan_submitted_counts_as_waiting() {
    // `notify_human_gate` reads the vault -- isolate $HOME so this can never
    // touch a developer's real vault or fire a real Telegram message, same
    // reasoning as the Telegram-callback tests.
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        let auth_conn = test_auth_conn();

        let item_id = seed_ready_item(&mcp, Some("claude-code"));
        mcp.with_backend_db(|conn| {
            agentflare_backend::item::update(
                conn,
                &item_id,
                agentflare_backend::item::UpdateItem {
                    // plan_required with no plan_status at all -> Blocked("none").
                    metadata: Some(r#"{"plan_required":true}"#.into()),
                    ..Default::default()
                },
            )
            .unwrap();
        })
        .unwrap();

        let result = run_discovery_tick(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
        );

        assert_eq!(
            result.dispatched, 0,
            "an unsubmitted plan must not dispatch"
        );
        assert_eq!(
            result.waiting, 1,
            "a never-submitted plan gate must be counted as waiting, not silently dropped"
        );
        assert_eq!(result.skipped, 0);
        assert!(
            queue.list(None).unwrap().is_empty(),
            "nothing may be enqueued for a plan-gated item"
        );
    });
}

#[test]
fn run_review_sweep_ignores_items_not_in_review() {
    let mcp = test_mcp();
    let queue = test_queue();
    let _item_id = seed_ready_item(&mcp, Some("claude-code"));

    let auth_conn = test_auth_conn();
    let result = run_review_sweep(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.promoted, 0);
    assert_eq!(result.self_repaired, 0);
    assert_eq!(result.skipped, 0);
}

#[test]
fn run_review_sweep_skips_an_item_whose_pr_status_cannot_be_determined() {
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let queue = test_queue();
    let _item_id = seed_in_review_item(&mcp, Some("claude-code"));

    let auth_conn = test_auth_conn();
    let result = run_review_sweep(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.promoted, 0);
    assert_eq!(result.self_repaired, 0);
    assert_eq!(result.skipped, 1);
    assert!(queue.list(None).unwrap().is_empty());
}

// Task #198: an item carrying `metadata.pr.number` takes the batched-GraphQL
// fetch path instead of the old one-REST-call-per-item path. `throwaway_repo`
// has no GitHub remote at all, so `RepoId::resolve_from_remote` fails before
// any network call would even be attempted either way -- what this pins is
// that the *new* numbered/batch code path degrades the same way the
// pre-existing unnumbered path already does (`Unknown` -> `skipped`, no
// panic), for an item shape (`metadata.pr.number` set) none of the other
// `run_review_sweep` tests above exercise.
#[test]
fn run_review_sweep_skips_a_numbered_item_the_same_way_when_no_remote_resolves() {
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let queue = test_queue();
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    mcp.with_backend_db(|conn| {
        agentflare_backend::item::update(
            conn,
            &item_id,
            agentflare_backend::item::UpdateItem {
                metadata: Some(r#"{"pr":{"number":501,"branch":"task/501"}}"#.into()),
                ..Default::default()
            },
        )
        .unwrap();
    })
    .unwrap();

    let auth_conn = test_auth_conn();
    let result = run_review_sweep(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.promoted, 0);
    assert_eq!(result.self_repaired, 0);
    assert_eq!(result.skipped, 1);
    assert!(queue.list(None).unwrap().is_empty());
}

#[path = "supervisor/tests/stray_pr_tests.rs"]
mod stray_pr_tests;

#[test]
fn run_review_sweep_scans_in_review_items_from_every_registered_project_not_just_one() {
    // Item #124: review sweep used to resolve a single project via
    // `mcp.resolve_project` (whatever project this daemon process's own
    // `mcp` happens to be linked to) and silently never looked at any other
    // registered project's in-review items -- mirrors item #63's fix for
    // `run_discovery_tick`.
    let repo_a = throwaway_repo();
    let repo_b = throwaway_repo();
    let mcp = test_mcp_with_repo(repo_a.path().to_path_buf());
    let queue = test_queue();
    // Seeding through `mcp` links its own cwd-resolved project into
    // `project_dirs` at `repo_a`'s path -- the pre-#124 behavior would only
    // ever have scanned this one.
    let _item_a = seed_in_review_item(&mcp, Some("claude-code"));
    let _item_b = seed_in_review_item_in_project(&mcp, "proj-b", &repo_b.path().to_string_lossy());

    let auth_conn = test_auth_conn();
    let result = run_review_sweep(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    // Neither throwaway repo has a real GitHub remote, so both items'
    // PR/CI status comes back `Unknown` -- the assertion that matters here
    // is the *count*: both projects' items must have been polled at all.
    assert_eq!(
        result.skipped, 2,
        "both projects' in-review items must be scanned, not just one"
    );
}

// --- auto-merge on CI-green + approval label (item #194) ---

#[test]
fn merge_approved_pr_merges_via_squash_on_success() {
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(200, r#"{"merged":true}"#),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };

    assert!(merge_approved_pr(&client, &repo, 42));

    let reqs = server.requests();
    assert_eq!(reqs[0].method, "PUT");
    assert_eq!(reqs[0].path, "/repos/o/r/pulls/42/merge");
    let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
    assert_eq!(sent["merge_method"], "squash");
}

#[test]
fn merge_approved_pr_returns_false_and_does_not_panic_on_github_error() {
    // Branch protection / an unresolved conflict -- GitHub answers 405 on
    // the merge endpoint. The safety property is that this falls through to
    // `skipped` (no panic, no retry loop here); the sweep just polls again
    // next tick.
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(405, r#"{"message":"not mergeable"}"#),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };

    assert!(!merge_approved_pr(&client, &repo, 42));
}

#[test]
fn merge_if_approved_skips_without_touching_network_when_label_is_absent() {
    // No approval label on the PR -- CI green alone must never be enough to
    // merge. The label check must happen before any GitHub call, so this
    // must return false even with an unresolvable repo/no credentials.
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();

    let merged = merge_if_approved(&mcp, &item, repo.path(), 42, &["size/s".to_string()]);

    assert!(!merged);
    let still_in_review = mcp
        .with_backend_db(|conn| {
            let refetched = agentflare_backend::item::get(conn, &item_id).unwrap();
            let state = agentflare_backend::state::get(conn, &refetched.state_id).unwrap();
            state.group_name == "in_review"
        })
        .unwrap();
    assert!(still_in_review, "an unapproved item must not be promoted");
}

#[test]
fn run_review_sweep_never_merges_when_the_approval_label_only_exists_on_the_project_not_the_pr() {
    // Regression for the safety property in item #194's spec: the approval
    // label must gate on the PR's OWN GitHub labels (carried by
    // `PrCiStatus::Passing`), never merely on the label existing somewhere
    // in the project's label table. A throwaway repo with no remote always
    // resolves to `PrCiStatus::Unknown`, so this also covers Pending/Failing
    // by construction -- none of those variants carry PR labels for
    // `merge_if_approved` to check in the first place.
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let _item_id = seed_in_review_item(&mcp, Some("claude-code"));
    mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).unwrap();
        agentflare_backend::label::create(
            conn,
            agentflare_backend::label::CreateLabel {
                project_id: Some(project.id.clone()),
                workspace_id: project.workspace_id.clone(),
                name: PR_APPROVAL_LABEL.into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
    })
    .unwrap();
    let queue = test_queue();
    let auth_conn = test_auth_conn();

    let result = run_review_sweep(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.promoted, 0);
    assert_eq!(result.skipped, 1);
}

// --- cross-machine self-repair claim arbitration (item #261) ---

/// Wraps a marker comment body the way `MockResponse::json`'s callers need
/// it -- serialized as a single JSON array entry with `id`/`user`/`body`,
/// matching `github::models::Comment`'s shape.
fn comment_json(id: u64, body: &str) -> String {
    serde_json::json!([{"id": id, "user": {"login": "claude-code"}, "body": body}]).to_string()
}

fn claim_marker(owner: &str, ts: i64) -> String {
    crate::github::bridge::marker::Marker {
        action: crate::github::bridge::marker::Action::Claim,
        owner: owner.to_string(),
        item: "i1".to_string(),
        ts,
        hash: String::new(),
    }
    .render()
}

#[test]
fn claim_self_repair_wins_and_posts_a_marker_when_the_pr_has_no_live_claim() {
    // `machine_label()` reads/creates a real `~/.agentflare/bridge-instance-id`
    // -- not mockable -- so the post-claim read must echo back THIS
    // process's own real owner value, not a fixed placeholder.
    let me = crate::github::bridge::config::machine_label();
    let server = crate::github::test_support::MockServer::start(vec![
        // Pre-claim read: no marker comments at all yet.
        crate::github::test_support::MockResponse::json(200, r#"[]"#),
        crate::github::test_support::MockResponse::json(201, r#"{"id":42}"#),
        // Post-claim read: only our own marker is present now.
        crate::github::test_support::MockResponse::json(
            200,
            &comment_json(42, &claim_marker(&me, 1_000)),
        ),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };

    assert!(claim_self_repair(&client, &repo, 688, "i1", 1_000));

    let reqs = server.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0].method, "GET");
    assert_eq!(reqs[1].method, "POST");
    assert!(
        reqs[1].body.contains("action=claim") && reqs[1].body.contains(&me),
        "must post a claim marker under our own owner id, got {:?}",
        reqs[1].body
    );
    assert_eq!(reqs[2].method, "GET");
}

#[test]
fn claim_self_repair_defers_when_another_workstation_already_holds_a_live_claim() {
    // Regression for item #261's live incident: PR #688 got self-repaired
    // (and beacon-labeled) independently by two workstations. This simulates
    // the second workstation's check -- a live claim from a different
    // `owner` must make it back off instead of dispatching its own repair.
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(
            200,
            &comment_json(1, &claim_marker("flared:c997d745ae66", 1_000)),
        ),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };

    assert!(!claim_self_repair(&client, &repo, 688, "i1", 1_050));

    // Must back off WITHOUT posting a second claim marker -- only the initial
    // read is expected; a second mock response is deliberately not queued,
    // so the test would panic (out-of-responses) if it tried to claim anyway.
    let reqs = server.requests();
    assert_eq!(reqs.len(), 1);
}

#[test]
fn claim_self_repair_proceeds_when_the_only_existing_claim_has_gone_stale() {
    // The TTL side of the same arbitration: a claim recorded far enough in
    // the past (past `self_repair_claim_ttl_secs()`) must not block a fresh
    // attempt -- e.g. the original claimant's workstation died mid-repair.
    let ttl = self_repair_claim_ttl_secs();
    let me = crate::github::bridge::config::machine_label();
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(
            200,
            &comment_json(1, &claim_marker("flared:dead", 10_000 - ttl - 1)),
        ),
        crate::github::test_support::MockResponse::json(201, r#"{"id":2}"#),
        crate::github::test_support::MockResponse::json(
            200,
            &comment_json(2, &claim_marker(&me, 10_000)),
        ),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };

    assert!(claim_self_repair(&client, &repo, 688, "i1", 10_000));
    assert_eq!(server.requests().len(), 3);
}

#[test]
fn self_repair_or_gate_dispatches_a_job_and_posts_a_marker_comment() {
    let mcp = test_mcp();
    let queue = test_queue();
    // Claim backdated past the (default 1800s) in_review TTL cap -- the
    // "prior job's lease has genuinely gone stale" case, where a
    // self-repair dispatch has a real chance to acquire the item.
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let label_id_by_name = seed_gate_label(&mcp);
    let auth_conn = test_auth_conn();

    let outcome = self_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        &["clippy".to_string()],
        &[],
        &label_id_by_name,
        "/repo",
    );

    assert!(matches!(outcome, SelfRepairOutcome::Dispatched));
    let jobs = queue.list(None).unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].args.contains(&item_id));
    assert!(
        jobs[0].args.contains(&"/repo".to_string()),
        "item #124: the enqueued job must carry the item's own project folder_path, not \
         resolve it later from the daemon's ambient cwd, got {:?}",
        jobs[0].args
    );
    assert_eq!(
        jobs[0].dispatch_reason.as_deref(),
        Some("self-repair: clippy"),
        "dashboard needs to badge why this job was fired, not just that it was"
    );
    let comments = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id).unwrap())
        .unwrap();
    assert!(
        comments
            .iter()
            .any(|c| c.body.starts_with(CI_SELF_REPAIR_MARKER))
    );
}

#[test]
fn self_repair_or_gate_gates_instead_of_dispatching_once_the_cap_is_reached() {
    // The cap-reached branch now fires a best-effort Telegram notify -- run
    // under an isolated home so this can't read (or send through) the
    // developer's real vault, same reasoning as channels.rs's own
    // vault-touching tests.
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item(&mcp, Some("claude-code"));
        let label_id_by_name = seed_gate_label(&mcp);
        let auth_conn = test_auth_conn();

        // Pre-seed SELF_REPAIR_CAP prior marker comments -- as if this many
        // repair rounds already ran with CI still red.
        for _ in 0..crate::quota::decide::SELF_REPAIR_CAP {
            mcp.comment_impl(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.clone()),
                body: Some(format!("{CI_SELF_REPAIR_MARKER}\n\njob: prior")),
                ..Default::default()
            })
            .unwrap();
        }
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap();

        let outcome = self_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            1,
            &["clippy".to_string()],
            &[],
            &label_id_by_name,
            "/repo",
        );

        assert!(matches!(outcome, SelfRepairOutcome::Skipped));
        assert!(queue.list(None).unwrap().is_empty());
        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(labels_contain_name(&mcp, &labels, NEEDS_HUMAN_GATE_LABEL));
    });
}

#[test]
fn self_repair_or_gate_stays_quiet_once_already_gated() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let label_id_by_name = seed_gate_label(&mcp);
    mcp.item_add_label(ItemRequest {
        action: "add_label".into(),
        id: Some(item_id.clone()),
        label_id: Some(label_id_by_name[NEEDS_HUMAN_GATE_LABEL].clone()),
        ..Default::default()
    })
    .unwrap();
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let auth_conn = test_auth_conn();

    let outcome = self_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        &["clippy".to_string()],
        &[],
        &label_id_by_name,
        "/repo",
    );

    assert!(matches!(outcome, SelfRepairOutcome::Skipped));
    assert!(queue.list(None).unwrap().is_empty());
}

#[test]
fn self_repair_or_gate_still_skips_gracefully_when_unassigned_and_no_router_rule_matches() {
    // The router fallback (item #19 regression) must degrade the same way
    // the pre-existing "no assignee" path always did when there's genuinely
    // nothing to route to (no `~/.agentflare/config.toml`, the common case)
    // -- Skipped, not a panic, not a double-dispatch.
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        // Backdated past the claim TTL, same as the dispatch-success test --
        // otherwise item #114's claim-liveness check (Deferred) would return
        // first and this would never reach the assignee-resolution branch.
        let item_id = seed_in_review_item_with_claim_age(&mcp, None, 1_900);
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap();
        let label_id_by_name = seed_gate_label(&mcp);
        let auth_conn = test_auth_conn();

        let outcome = self_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            1,
            &["clippy".to_string()],
            &[],
            &label_id_by_name,
            "/repo",
        );

        assert!(matches!(outcome, SelfRepairOutcome::Skipped));
        assert!(queue.list(None).unwrap().is_empty());
    });
}

#[test]
fn self_repair_or_gate_does_not_double_dispatch_while_a_job_is_already_in_flight() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let label_id_by_name = seed_gate_label(&mcp);
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let auth_conn = test_auth_conn();
    let job = agentflare_jobs::AgentJob::new("agentflare-work")
        .args([item_id.clone(), "claude-code".to_string()])
        .in_process();
    queue.enqueue(&job).unwrap();

    let outcome = self_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        &["clippy".to_string()],
        &[],
        &label_id_by_name,
        "/repo",
    );

    assert!(matches!(outcome, SelfRepairOutcome::Skipped));
    assert_eq!(
        queue.list(None).unwrap().len(),
        1,
        "must not enqueue a second job"
    );
}

#[test]
fn self_repair_or_gate_defers_instead_of_dispatching_into_a_still_live_claim() {
    // Item #114: the original job's claim is still within its (#108-capped)
    // in_review TTL -- nobody can actually reclaim the item yet, so a
    // self-repair dispatch here would just die instantly at its own
    // claim-acquire step. Must defer, not dispatch and not count against
    // the self-repair cap.
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let label_id_by_name = seed_gate_label(&mcp);
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let auth_conn = test_auth_conn();

    let outcome = self_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        &["clippy".to_string()],
        &[],
        &label_id_by_name,
        "/repo",
    );

    assert!(matches!(outcome, SelfRepairOutcome::Deferred));
    assert!(
        queue.list(None).unwrap().is_empty(),
        "must not dispatch a job into a claim that can't be won yet"
    );
    let comments = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id).unwrap())
        .unwrap();
    assert!(
        !comments
            .iter()
            .any(|c| c.body.starts_with(CI_SELF_REPAIR_MARKER)),
        "a deferred attempt must not post a self-repair-dispatched marker, \
         or it would count against the cap on a later real attempt"
    );
}

// Regression for item #273's follow-up: a PR heading into CI self-repair
// isn't always leaving plain in-review -- it might be leaving CodeRabbit's
// own review-repair stage instead, and removing a label that isn't there is
// a silent no-op, so the wrong `from` label left both stacked on the PR.
#[test]
fn stale_stage_label_picks_the_coderabbit_review_repair_label_when_present() {
    let labels = vec![CODERABBIT_REPAIR_PR_LABEL.to_string()];
    assert_eq!(stale_stage_label(&labels), Some(CODERABBIT_REPAIR_PR_LABEL));
}

#[test]
fn stale_stage_label_falls_back_to_the_in_review_label() {
    let labels = vec![IN_REVIEW_PR_LABEL.to_string()];
    assert_eq!(stale_stage_label(&labels), Some(IN_REVIEW_PR_LABEL));
}

#[test]
fn stale_stage_label_is_none_when_neither_stage_label_is_present() {
    assert_eq!(stale_stage_label(&[]), None);
}

// --- coderabbit_repair_or_gate (item #273) ---

fn coderabbit_finding(id: u64, login: &str) -> crate::github::models::ReviewComment {
    crate::github::models::ReviewComment {
        id,
        user: crate::github::models::User {
            login: login.to_string(),
        },
        path: "src/lib.rs".to_string(),
        line: Some(42),
        body: "this could panic on an empty slice".to_string(),
    }
}

#[test]
fn unresolved_coderabbit_comments_keeps_only_unresolved_coderabbit_findings() {
    let comments = vec![
        coderabbit_finding(1, "coderabbitai[bot]"),
        coderabbit_finding(2, "coderabbitai[bot]"),
        coderabbit_finding(3, "a-human-reviewer"),
    ];
    let mut resolved = std::collections::HashSet::new();
    resolved.insert(2u64); // this CodeRabbit comment's thread was resolved

    let unresolved = unresolved_coderabbit_comments(&comments, &resolved);

    assert_eq!(
        unresolved.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![1],
        "must drop the resolved CodeRabbit comment and the human reviewer's \
         comment, keeping only the still-unresolved CodeRabbit one"
    );
}

#[test]
fn unresolved_coderabbit_comments_matches_the_bot_login_case_insensitively() {
    let comments = vec![coderabbit_finding(1, "CodeRabbitAI[bot]")];
    let unresolved = unresolved_coderabbit_comments(&comments, &std::collections::HashSet::new());
    assert_eq!(unresolved.len(), 1);
}

#[test]
fn coderabbit_repair_or_gate_skips_without_touching_anything_when_there_are_no_findings() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let label_id_by_name = seed_gate_label(&mcp);
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let auth_conn = test_auth_conn();

    let outcome = coderabbit_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        &[],
        &[],
        &label_id_by_name,
        "/repo",
    );

    assert!(matches!(outcome, SelfRepairOutcome::Skipped));
    assert!(queue.list(None).unwrap().is_empty());
    let comments = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id).unwrap())
        .unwrap();
    assert!(comments.is_empty(), "a clean review must stay silent");
}

#[test]
fn coderabbit_repair_or_gate_dispatches_a_job_and_posts_a_summary_of_the_findings() {
    let mcp = test_mcp();
    let queue = test_queue();
    // Claim backdated past the in_review TTL cap, same trick
    // `self_repair_or_gate_dispatches_a_job_and_posts_a_marker_comment` uses,
    // so the claim-liveness gate doesn't short-circuit before dispatch.
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let label_id_by_name = seed_gate_label(&mcp);
    let auth_conn = test_auth_conn();
    let findings = vec![
        coderabbit_finding(1, "coderabbitai[bot]"),
        coderabbit_finding(2, "coderabbitai[bot]"),
    ];

    let outcome = coderabbit_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        &findings,
        &[],
        &label_id_by_name,
        "/repo",
    );

    assert!(matches!(outcome, SelfRepairOutcome::Dispatched));
    let jobs = queue.list(None).unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].args.contains(&item_id));
    assert_eq!(
        jobs[0].dispatch_reason.as_deref(),
        Some("CodeRabbit review: 2 unresolved finding(s)")
    );
    let comments = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id).unwrap())
        .unwrap();
    let dispatch_comment = comments
        .iter()
        .find(|c| c.body.starts_with(CODERABBIT_REPAIR_MARKER))
        .expect("must post a marker comment summarizing what's being addressed");
    assert!(dispatch_comment.body.contains("src/lib.rs:42"));
    assert!(
        dispatch_comment
            .body
            .contains("this could panic on an empty slice")
    );
}

#[test]
fn coderabbit_repair_or_gate_gates_instead_of_dispatching_once_the_cap_is_reached() {
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item(&mcp, Some("claude-code"));
        let label_id_by_name = seed_gate_label(&mcp);

        for _ in 0..crate::quota::decide::SELF_REPAIR_CAP {
            mcp.comment_impl(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.clone()),
                body: Some(format!("{CODERABBIT_REPAIR_MARKER}\n\njob: prior")),
                ..Default::default()
            })
            .unwrap();
        }
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap();
        let auth_conn = test_auth_conn();
        let findings = vec![coderabbit_finding(1, "coderabbitai[bot]")];

        let outcome = coderabbit_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            1,
            &findings,
            &[],
            &label_id_by_name,
            "/repo",
        );

        assert!(matches!(outcome, SelfRepairOutcome::Skipped));
        assert!(queue.list(None).unwrap().is_empty());
        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(labels_contain_name(&mcp, &labels, NEEDS_HUMAN_GATE_LABEL));
    });
}

#[test]
fn coderabbit_repair_or_gate_stays_quiet_once_already_gated() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let label_id_by_name = seed_gate_label(&mcp);
    mcp.item_add_label(ItemRequest {
        action: "add_label".into(),
        id: Some(item_id.clone()),
        label_id: Some(label_id_by_name[NEEDS_HUMAN_GATE_LABEL].clone()),
        ..Default::default()
    })
    .unwrap();
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let auth_conn = test_auth_conn();
    let findings = vec![coderabbit_finding(1, "coderabbitai[bot]")];

    let outcome = coderabbit_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        &findings,
        &[],
        &label_id_by_name,
        "/repo",
    );

    assert!(matches!(outcome, SelfRepairOutcome::Skipped));
    assert!(queue.list(None).unwrap().is_empty());
}

#[test]
fn coderabbit_repair_or_gate_does_not_double_dispatch_while_a_job_is_already_in_flight() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let label_id_by_name = seed_gate_label(&mcp);
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let auth_conn = test_auth_conn();
    let job = agentflare_jobs::AgentJob::new("agentflare-work")
        .args([item_id.clone(), "claude-code".to_string()])
        .in_process();
    queue.enqueue(&job).unwrap();
    let findings = vec![coderabbit_finding(1, "coderabbitai[bot]")];

    let outcome = coderabbit_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        &findings,
        &[],
        &label_id_by_name,
        "/repo",
    );

    assert!(matches!(outcome, SelfRepairOutcome::Skipped));
    assert_eq!(
        queue.list(None).unwrap().len(),
        1,
        "must not enqueue a second job"
    );
}

#[test]
fn coderabbit_repair_or_gate_reverts_the_stage_label_once_findings_are_resolved() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let label_id_by_name = seed_gate_label(&mcp);
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let auth_conn = test_auth_conn();

    // No findings left, but the PR is still carrying the review-repair stage
    // label from a prior dispatch -- must skip (no job to enqueue), and the
    // revert-label call is exercised (best-effort against "/repo", which has
    // no resolvable remote, so it's a silent no-op here rather than a panic).
    let outcome = coderabbit_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        &[],
        &[CODERABBIT_REPAIR_PR_LABEL.to_string()],
        &label_id_by_name,
        "/repo",
    );

    assert!(matches!(outcome, SelfRepairOutcome::Skipped));
    assert!(queue.list(None).unwrap().is_empty());
}

#[test]
fn first_time_gated_is_true_once_then_false_for_the_same_id() {
    // Unique per-test id -- the backing set is a single process-wide static
    // shared by every test in this binary, so a literal like "item-1" would
    // collide with another test using the same id.
    let id = "first-time-gated-test-item-9f3a";
    assert!(
        first_time_gated(id),
        "the first sighting of a newly-gated item must notify"
    );
    assert!(
        !first_time_gated(id),
        "a later tick re-seeing the same still-gated item must not notify again"
    );
}

#[test]
fn first_time_gated_treats_different_ids_independently() {
    let a = "first-time-gated-test-item-a1";
    let b = "first-time-gated-test-item-b2";
    assert!(first_time_gated(a));
    assert!(
        first_time_gated(b),
        "a different item id must still get its own first-sighting notify"
    );
}

/// Sets up a project with the `ready-for-work` label and an item, optionally
/// with dependencies and an assignee -- shared by the `cascade_unblock_dependents`
/// tests below.
fn seed_item_with_deps(
    mcp: &AgentflareMcp,
    name: &str,
    assignee: Option<&str>,
    dependency_ids: Vec<String>,
) -> String {
    mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).unwrap();
        if agentflare_backend::label::list_by_project(conn, &project.id)
            .unwrap()
            .iter()
            .all(|l| l.name != READY_LABEL)
        {
            agentflare_backend::label::create(
                conn,
                agentflare_backend::label::CreateLabel {
                    project_id: Some(project.id.clone()),
                    workspace_id: project.workspace_id.clone(),
                    name: READY_LABEL.into(),
                    color: None,
                    parent_id: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap();
        }
        let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
        let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
        agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project.id,
                state_id,
                name: name.into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: assignee.map(str::to_string),
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids,
                start_date: None,
                due_date: None,
            },
        )
        .unwrap()
        .id
    })
    .unwrap()
}

fn complete_item(mcp: &AgentflareMcp, item_id: &str) {
    mcp.with_backend_db(|conn| {
        let item = agentflare_backend::item::get(conn, item_id).unwrap();
        let completed =
            agentflare_backend::state::first_in_group(conn, &item.project_id, "completed").unwrap();
        agentflare_backend::item::update_state(conn, item_id, &completed.id).unwrap();
    })
    .unwrap();
}

fn item_has_ready_label(mcp: &AgentflareMcp, item_id: &str) -> bool {
    // Two sequential with_backend_db calls, not nested -- the backing
    // connection lock is a plain (non-reentrant) Mutex.
    let labels = mcp
        .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, item_id).unwrap())
        .unwrap();
    labels_contain_name(mcp, &labels, READY_LABEL)
}

#[test]
fn cascade_unblock_dependents_labels_dependent_once_its_only_dependency_completes() {
    let mcp = test_mcp();
    let blocker = seed_item_with_deps(&mcp, "Blocker", None, vec![]);
    let dependent = seed_item_with_deps(
        &mcp,
        "Dependent",
        Some("claude-code"),
        vec![blocker.clone()],
    );
    complete_item(&mcp, &blocker);

    mcp.with_backend_db(|conn| cascade_unblock_dependents(conn, &blocker))
        .unwrap();

    assert!(
        item_has_ready_label(&mcp, &dependent),
        "dependent's only dependency is completed -- it must be auto-labeled ready-for-work"
    );
}

#[test]
fn cascade_unblock_dependents_leaves_dependent_with_a_still_open_sibling_dependency() {
    let mcp = test_mcp();
    let blocker_a = seed_item_with_deps(&mcp, "BlockerA", None, vec![]);
    let blocker_b = seed_item_with_deps(&mcp, "BlockerB", None, vec![]);
    let dependent = seed_item_with_deps(
        &mcp,
        "Dependent",
        Some("claude-code"),
        vec![blocker_a.clone(), blocker_b.clone()],
    );
    complete_item(&mcp, &blocker_a);

    mcp.with_backend_db(|conn| cascade_unblock_dependents(conn, &blocker_a))
        .unwrap();

    assert!(
        !item_has_ready_label(&mcp, &dependent),
        "blocker_b is still open -- the dependent must not be auto-labeled yet"
    );
}

#[test]
fn cascade_unblock_dependents_skips_a_dependent_when_completed_item_has_no_assignee_either() {
    let mcp = test_mcp();
    let blocker = seed_item_with_deps(&mcp, "Blocker", None, vec![]);
    let dependent = seed_item_with_deps(&mcp, "Dependent", None, vec![blocker.clone()]);
    complete_item(&mcp, &blocker);

    mcp.with_backend_db(|conn| cascade_unblock_dependents(conn, &blocker))
        .unwrap();

    assert!(
        !item_has_ready_label(&mcp, &dependent),
        "an unassigned dependent has nothing to inherit from an equally-unassigned \
         completed blocker -- it must not be silently auto-labeled"
    );
}

#[test]
fn cascade_unblock_dependents_unassigned_dependent_inherits_completed_items_assignee() {
    let mcp = test_mcp();
    let blocker = seed_item_with_deps(&mcp, "Blocker", Some("claude-code:instance-1"), vec![]);
    let dependent = seed_item_with_deps(&mcp, "Dependent", None, vec![blocker.clone()]);
    complete_item(&mcp, &blocker);

    mcp.with_backend_db(|conn| cascade_unblock_dependents(conn, &blocker))
        .unwrap();

    assert!(
        item_has_ready_label(&mcp, &dependent),
        "dependent had no assignee -- it should inherit the completed item's and get labeled"
    );
    let dependent_assignee = mcp
        .with_backend_db(|conn| {
            agentflare_backend::item::get(conn, &dependent)
                .unwrap()
                .assignee_agent
        })
        .unwrap();
    assert_eq!(
        dependent_assignee.as_deref(),
        Some("claude-code"),
        "inherited assignee must be stripped down to the bare agent id, not the \
         agent:instance form"
    );
}

#[test]
fn cascade_unblock_dependents_is_idempotent_across_repeated_calls() {
    let mcp = test_mcp();
    let blocker = seed_item_with_deps(&mcp, "Blocker", None, vec![]);
    let dependent = seed_item_with_deps(
        &mcp,
        "Dependent",
        Some("claude-code"),
        vec![blocker.clone()],
    );
    complete_item(&mcp, &blocker);

    mcp.with_backend_db(|conn| cascade_unblock_dependents(conn, &blocker))
        .unwrap();
    mcp.with_backend_db(|conn| cascade_unblock_dependents(conn, &blocker))
        .unwrap();

    let labels = mcp
        .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &dependent).unwrap())
        .unwrap();
    let ready_count = mcp
        .with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            let ready_id = agentflare_backend::label::list_by_project(conn, &project.id)
                .unwrap()
                .into_iter()
                .find(|l| l.name == READY_LABEL)
                .unwrap()
                .id;
            labels.iter().filter(|id| **id == ready_id).count()
        })
        .unwrap();
    assert_eq!(
        ready_count, 1,
        "add_label's INSERT OR IGNORE must keep repeated cascade calls idempotent"
    );
}

/// Regression test for the typed-relations migration (0012): a `duplicate`
/// relation with no `blocks` edge must never be treated as a blocking
/// dependency by `cascade_unblock_dependents` -- `dependents_of` only reads
/// `relation_type = 'blocks'` rows.
#[test]
fn cascade_unblock_dependents_ignores_a_pure_duplicate_relation() {
    let mcp = test_mcp();
    let a = seed_item_with_deps(&mcp, "A", None, vec![]);
    let b = seed_item_with_deps(&mcp, "B", Some("claude-code"), vec![]);
    mcp.with_backend_db(|conn| {
        agentflare_backend::item::add_relation(conn, &b, &a, "duplicate").unwrap()
    })
    .unwrap();
    complete_item(&mcp, &a);

    mcp.with_backend_db(|conn| cascade_unblock_dependents(conn, &a))
        .unwrap();

    assert!(
        !item_has_ready_label(&mcp, &b),
        "a duplicate-only relation must not be read as a blocking dependency"
    );
}

/// A `duplicate` relation coexisting alongside a real `blocks` edge between
/// the same pair must not change `cascade_unblock_dependents`'s behavior
/// for the `blocks` edge -- the load-bearing constraint from item #3's spec
/// §3.
#[test]
fn cascade_unblock_dependents_unaffected_by_coexisting_duplicate_relation() {
    let mcp = test_mcp();
    let blocker = seed_item_with_deps(&mcp, "Blocker", None, vec![]);
    let dependent = seed_item_with_deps(
        &mcp,
        "Dependent",
        Some("claude-code"),
        vec![blocker.clone()],
    );
    mcp.with_backend_db(|conn| {
        agentflare_backend::item::add_relation(conn, &dependent, &blocker, "duplicate").unwrap()
    })
    .unwrap();
    complete_item(&mcp, &blocker);

    mcp.with_backend_db(|conn| cascade_unblock_dependents(conn, &blocker))
        .unwrap();

    assert!(
        item_has_ready_label(&mcp, &dependent),
        "a coexisting duplicate relation must not suppress the real blocks-edge cascade"
    );
}
