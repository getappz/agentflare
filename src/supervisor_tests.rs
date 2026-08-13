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
    let mcp = test_mcp();
    let queue = test_queue();
    let (item_id, _goal_id) =
        seed_ready_item_under_active_goal_with_repairs(&mcp, crate::quota::decide::SELF_REPAIR_CAP);

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
    // cursor has a REGISTRY entry but no autonomous_args mapped, so it's
    // still "unconfirmed" for autonomous dispatch (unlike opencode, now
    // that autonomous_args(Agent::Opencode) is Some(&["--auto"])).
    let item_id = seed_ready_item(&mcp, Some("cursor"));

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
/// already covered by `mcp_server::tests`.
fn seed_in_review_item(mcp: &AgentflareMcp, assignee: Option<&str>) -> String {
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
            },
        )
        .unwrap();
        let owner = assignee
            .map(|a| format!("{a}:prior-job"))
            .unwrap_or_else(|| "cli".into());
        agentflare_backend::item::claim(
            conn,
            &item.id,
            &owner,
            crate::claims::now(),
            crate::claims::ttl_secs(),
        )
        .unwrap();
        agentflare_backend::item::mark_in_review(conn, &item.id, &owner).unwrap();
        item.id
    })
    .unwrap()
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

#[test]
fn self_repair_or_gate_dispatches_a_job_and_posts_a_marker_comment() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
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
    );

    assert!(matches!(outcome, SelfRepairOutcome::Dispatched));
    let jobs = queue.list(None).unwrap();
    assert_eq!(jobs.len(), 1);
    assert!(jobs[0].args.contains(&item_id));
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
    );

    assert!(matches!(outcome, SelfRepairOutcome::Skipped));
    assert!(queue.list(None).unwrap().is_empty());
    let labels = mcp
        .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
        .unwrap();
    assert!(labels_contain_name(&mcp, &labels, NEEDS_HUMAN_GATE_LABEL));
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
    );

    assert!(matches!(outcome, SelfRepairOutcome::Skipped));
    assert_eq!(
        queue.list(None).unwrap().len(),
        1,
        "must not enqueue a second job"
    );
}
