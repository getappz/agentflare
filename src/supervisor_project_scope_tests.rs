//! Cross-project dispatch bookkeeping tests, split out of
//! `supervisor_tests.rs` (frozen at the LOC-gate limit, `scripts/loc-gate.sh`)
//! the same way `supervisor_telegram_tests.rs` is. `use super::*` keeps every
//! helper (`test_mcp`, `test_queue`, `test_auth_conn`, ...) available exactly
//! as in the parent file.

use super::*;

/// Regression for the 2026-09-16/17 dispatch storm: the daemon walks every
/// registered project's folder, but its `AgentflareMcp` is linked to just one
/// project (the cwd it was started in). The bookkeeping for any OTHER
/// project's item -- the ready-for-work -> dispatched swap and the
/// `## supervisor — dispatched` marker `dispatch_failure_ceiling` counts
/// cycles by -- used to go through project-scoped MCP entry points that
/// rejected the id, silently, so the item stayed ready-for-work with no
/// marker and was re-dispatched every tick, forever (image-qc item #273,
/// 1,367 jobs). The mcp here is linked to its own project; the item lives in
/// a second project that is only reachable through `project_dirs`.
#[test]
fn dispatch_bookkeeping_lands_on_items_of_projects_other_than_the_daemons_own() {
    let mcp = test_mcp();
    let queue = test_queue();
    let (item_id, ready_id, dispatched_id) = mcp
        .with_backend_db(|conn| {
            let own = mcp.resolve_project(conn).unwrap();
            let other = agentflare_backend::project::create(
                conn,
                agentflare_backend::project::CreateProject {
                    workspace_id: own.workspace_id.clone(),
                    name: "other-repo".into(),
                    identifier: "OTHER".into(),
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap();
            assert_ne!(other.id, own.id);
            agentflare_backend::project_dir::upsert(conn, &other.id, "/elsewhere/other-repo", 0)
                .unwrap();
            let mut label_ids = std::collections::HashMap::new();
            for name in ["ready-for-work", "dispatched"] {
                let label = agentflare_backend::label::create(
                    conn,
                    agentflare_backend::label::CreateLabel {
                        project_id: Some(other.id.clone()),
                        workspace_id: other.workspace_id.clone(),
                        name: name.into(),
                        color: None,
                        parent_id: None,
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                    },
                )
                .unwrap();
                label_ids.insert(name, label.id);
            }
            let states = agentflare_backend::state::list_by_project(conn, &other.id).unwrap();
            let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: other.id.clone(),
                    state_id,
                    name: "Do the other thing".into(),
                    description: Some("in a project this daemon is not linked to".into()),
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
            agentflare_backend::item::add_label(conn, &item.id, &label_ids["ready-for-work"])
                .unwrap();
            (
                item.id,
                label_ids["ready-for-work"].clone(),
                label_ids["dispatched"].clone(),
            )
        })
        .unwrap();

    let auth_conn = test_auth_conn();
    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );
    assert_eq!(result.dispatched, 1);

    let labels = mcp
        .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
        .unwrap();
    assert!(
        !labels.contains(&ready_id),
        "ready-for-work must come off, or the next tick re-dispatches the item forever"
    );
    assert!(labels.contains(&dispatched_id));
    let comments = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id).unwrap())
        .unwrap();
    assert!(
        comments.iter().any(|c| c
            .body
            .starts_with(crate::dispatch_failure_ceiling::DISPATCH_MARKER)),
        "the dispatch marker is what dispatch_failure_ceiling counts cycles by; without it the \
         ceiling can never trip"
    );
}

/// CodeRabbit finding on this PR: `run_discovery_tick` only requires
/// `READY_LABEL` to exist before batching a project (see `DISPATCHED_LABEL`'s
/// doc comment) -- a project that never got the `dispatched` label seeded
/// could otherwise enqueue a real job, remove `ready-for-work`, and leave the
/// item wearing neither label: invisible to the dashboard and to the next
/// discovery query, with no way back onto `ready-for-work` short of a human
/// relabeling it by hand. `dispatch_item` must resolve `DISPATCHED_LABEL`
/// before enqueueing, not merely at label-swap time, and leave the item
/// exactly as it found it when that label is missing.
#[test]
fn dispatch_item_declines_when_the_project_has_no_dispatched_label() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = mcp
        .with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            // Deliberately only "ready-for-work" -- no "dispatched" label.
            let ready = agentflare_backend::label::create(
                conn,
                agentflare_backend::label::CreateLabel {
                    project_id: Some(project.id.clone()),
                    workspace_id: project.workspace_id.clone(),
                    name: "ready-for-work".into(),
                    color: None,
                    parent_id: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap();
            let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
            let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id,
                    name: "Do the thing".into(),
                    description: Some("no dispatched label exists yet".into()),
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
            agentflare_backend::item::add_label(conn, &item.id, &ready.id).unwrap();
            item.id
        })
        .unwrap();

    let auth_conn = test_auth_conn();
    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );
    assert_eq!(
        result.dispatched, 0,
        "must not dispatch without a dispatched label to swap to"
    );
    assert!(
        queue.list(None).unwrap().is_empty(),
        "no job should be enqueued when the label swap can never complete"
    );

    let labels = mcp
        .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
        .unwrap();
    let ready_id = mcp
        .with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            agentflare_backend::label::list_by_project(conn, &project.id)
                .unwrap()
                .into_iter()
                .find(|l| l.name == "ready-for-work")
                .unwrap()
                .id
        })
        .unwrap();
    assert!(
        labels.contains(&ready_id),
        "ready-for-work must stay on so a human fixing the missing label sees the item again \
         on the very next tick"
    );
}
