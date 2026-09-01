use super::*;

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
        &["clippy".to_string()],
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
            &["clippy".to_string()],
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
        &["clippy".to_string()],
        &label_id_by_name,
        "/repo",
    );

    assert!(matches!(outcome, SelfRepairOutcome::Skipped));
    assert!(queue.list(None).unwrap().is_empty());
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
        &["clippy".to_string()],
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
        &["clippy".to_string()],
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
