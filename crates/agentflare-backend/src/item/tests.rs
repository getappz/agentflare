use super::*;
use crate::db;
use crate::project::{self, CreateProject};
use crate::workspace::{self, CreateWorkspace};

fn seed_project(conn: &Connection, suffix: &str) -> (String, String) {
    let ws = workspace::create(
        conn,
        CreateWorkspace {
            name: format!("Test{suffix}"),
            slug: format!("test{suffix}"),
            owner_agent: None,
            item_label: None,
        },
    )
    .unwrap();
    let proj = project::create(
        conn,
        CreateProject {
            workspace_id: ws.id.clone(),
            name: format!("Test{suffix}"),
            identifier: format!("T{suffix}"),
            external_source: None,
            external_id: None,
        },
    )
    .unwrap();
    let states = crate::state::list_by_project(conn, &proj.id).unwrap();
    let state_id = states
        .iter()
        .find(|s| s.is_default)
        .map(|s| s.id.clone())
        .unwrap();
    (proj.id, state_id)
}

#[test]
fn create_and_get() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = create(
        &conn,
        CreateItem {
            project_id: pid,
            state_id: sid,
            name: "Test Item".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    assert_eq!(item.name, "Test Item");
    assert_eq!(item.sequence_id, 1);
    let got = get(&conn, &item.id).unwrap();
    assert_eq!(got.id, item.id);
}

#[test]
fn sequence_increments() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let i1 = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid.clone(),
            name: "First".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    let i2 = create(
        &conn,
        CreateItem {
            project_id: pid,
            state_id: sid,
            name: "Second".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    assert_eq!(i1.sequence_id, 1);
    assert_eq!(i2.sequence_id, 2);
}

#[test]
fn list_by_project_scopes() {
    let conn = db::open_in_memory().unwrap();
    let (pid1, sid1) = seed_project(&conn, "1");
    let (pid2, _sid2) = seed_project(&conn, "2");
    create(
        &conn,
        CreateItem {
            project_id: pid1.clone(),
            state_id: sid1,
            name: "Item 1".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    assert_eq!(list_by_project(&conn, &pid1).unwrap().len(), 1);
    assert_eq!(list_by_project(&conn, &pid2).unwrap().len(), 0);
}

#[test]
fn list_by_project_paginated_pages_and_counts() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    for n in 0..5 {
        create(
            &conn,
            CreateItem {
                project_id: pid.clone(),
                state_id: sid.clone(),
                name: format!("Item {n}"),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: Some(n as f64),
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
    }

    let (page, total) = list_by_project_paginated(&conn, &pid, 2, 0).unwrap();
    assert_eq!(total, 5);
    assert_eq!(
        page.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
        vec!["Item 0", "Item 1"]
    );

    let (page, total) = list_by_project_paginated(&conn, &pid, 2, 2).unwrap();
    assert_eq!(total, 5);
    assert_eq!(
        page.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(),
        vec!["Item 2", "Item 3"]
    );

    // A page past the end still reports the true total, not zero.
    let (page, total) = list_by_project_paginated(&conn, &pid, 2, 10).unwrap();
    assert!(page.is_empty());
    assert_eq!(total, 5);

    assert!(list_by_project_paginated(&conn, &pid, -1, 0).is_err());
    assert!(list_by_project_paginated(&conn, &pid, 0, -1).is_err());
}

#[test]
fn add_and_remove_labels() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid,
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    let ws = crate::workspace::list(&conn)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let label = crate::label::create(
        &conn,
        crate::label::CreateLabel {
            project_id: Some(pid),
            workspace_id: ws.id,
            name: "bug".into(),
            color: None,
            parent_id: None,
            sort_order: None,
            external_source: None,
            external_id: None,
        },
    )
    .unwrap();
    add_label(&conn, &item.id, &label.id).unwrap();
    let labels = list_labels(&conn, &item.id).unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0], label.id);
    remove_label(&conn, &item.id, &label.id).unwrap();
    assert!(list_labels(&conn, &item.id).unwrap().is_empty());
}

fn workspace_by_slug(conn: &Connection, slug: &str) -> String {
    workspace::list(conn)
        .unwrap()
        .into_iter()
        .find(|w| w.slug == slug)
        .unwrap()
        .id
}

#[test]
fn add_label_rejects_label_from_another_project() {
    let conn = db::open_in_memory().unwrap();
    let (pid1, sid1) = seed_project(&conn, "1");
    let (pid2, _sid2) = seed_project(&conn, "2");
    let item = create(
        &conn,
        CreateItem {
            project_id: pid1,
            state_id: sid1,
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    let foreign = crate::label::create(
        &conn,
        crate::label::CreateLabel {
            project_id: Some(pid2),
            workspace_id: workspace_by_slug(&conn, "test2"),
            name: "bug".into(),
            color: None,
            parent_id: None,
            sort_order: None,
            external_source: None,
            external_id: None,
        },
    )
    .unwrap();
    let err = add_label(&conn, &item.id, &foreign.id).unwrap_err();
    assert!(matches!(err, crate::error::Error::Validation(_)));
    assert!(list_labels(&conn, &item.id).unwrap().is_empty());
}

#[test]
fn add_label_accepts_workspace_level_label_in_same_workspace() {
    let conn = db::open_in_memory().unwrap();
    let (pid1, sid1) = seed_project(&conn, "1");
    let item = create(
        &conn,
        CreateItem {
            project_id: pid1,
            state_id: sid1,
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    // Workspace-level label (project_id = None) in the item's workspace.
    let global = crate::label::create(
        &conn,
        crate::label::CreateLabel {
            project_id: None,
            workspace_id: workspace_by_slug(&conn, "test1"),
            name: "global".into(),
            color: None,
            parent_id: None,
            sort_order: None,
            external_source: None,
            external_id: None,
        },
    )
    .unwrap();
    add_label(&conn, &item.id, &global.id).unwrap();
    assert_eq!(list_labels(&conn, &item.id).unwrap().len(), 1);
}

#[test]
fn add_label_rejects_workspace_level_label_from_another_workspace() {
    let conn = db::open_in_memory().unwrap();
    let (pid1, sid1) = seed_project(&conn, "1");
    let (_pid2, _sid2) = seed_project(&conn, "2");
    let item = create(
        &conn,
        CreateItem {
            project_id: pid1,
            state_id: sid1,
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    // Workspace-level label (project_id = None) but in a *different* workspace.
    let foreign_global = crate::label::create(
        &conn,
        crate::label::CreateLabel {
            project_id: None,
            workspace_id: workspace_by_slug(&conn, "test2"),
            name: "global".into(),
            color: None,
            parent_id: None,
            sort_order: None,
            external_source: None,
            external_id: None,
        },
    )
    .unwrap();
    let err = add_label(&conn, &item.id, &foreign_global.id).unwrap_err();
    assert!(matches!(err, crate::error::Error::Validation(_)));
    assert!(list_labels(&conn, &item.id).unwrap().is_empty());
}

#[test]
fn add_and_remove_assignees() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = create(
        &conn,
        CreateItem {
            project_id: pid,
            state_id: sid,
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    add_assignee(&conn, &item.id, "agent:1").unwrap();
    add_assignee(&conn, &item.id, "agent:2").unwrap();
    let agents = list_assignees(&conn, &item.id).unwrap();
    assert_eq!(agents.len(), 2);
    remove_assignee(&conn, &item.id, "agent:1").unwrap();
    assert_eq!(list_assignees(&conn, &item.id).unwrap().len(), 1);
}

#[test]
fn add_and_remove_dependencies() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let i1 = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid.clone(),
            name: "A".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    let i2 = create(
        &conn,
        CreateItem {
            project_id: pid,
            state_id: sid,
            name: "B".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    let i1_id = i1.id.clone();
    let i2_id = i2.id.clone();
    add_dependency(&conn, &i2_id, &i1_id).unwrap();
    let deps = list_dependencies(&conn, &i2_id).unwrap();
    assert_eq!(deps, vec![i1_id.clone()]);
    remove_dependency(&conn, &i2_id, &i1_id).unwrap();
    assert!(list_dependencies(&conn, &i2.id).unwrap().is_empty());
}

#[test]
fn create_wires_up_label_assignee_and_dependency_ids() {
    // Regression test: CreateItem.label_ids/assignee_ids/dependency_ids
    // must actually be attached by create(), not silently dropped.
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let ws = crate::workspace::list(&conn)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let label = crate::label::create(
        &conn,
        crate::label::CreateLabel {
            project_id: Some(pid.clone()),
            workspace_id: ws.id,
            name: "bug".into(),
            color: None,
            parent_id: None,
            sort_order: None,
            external_source: None,
            external_id: None,
        },
    )
    .unwrap();
    let blocker = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid.clone(),
            name: "Blocker".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    let item = create(
        &conn,
        CreateItem {
            project_id: pid,
            state_id: sid,
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
            sort_order: None,
            external_source: None,
            external_id: None,
            metadata: None,
            label_ids: vec![label.id.clone()],
            assignee_ids: vec!["agent:1".into()],
            dependency_ids: vec![blocker.id.clone()],
            start_date: None,
            due_date: None,
        },
    )
    .unwrap();
    assert_eq!(list_labels(&conn, &item.id).unwrap(), vec![label.id]);
    assert_eq!(
        list_assignees(&conn, &item.id).unwrap(),
        vec!["agent:1".to_string()]
    );
    assert_eq!(
        list_dependencies(&conn, &item.id).unwrap(),
        vec![blocker.id]
    );
}

#[test]
fn dependents_of_finds_reverse_edges() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let blocker = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid.clone(),
            name: "Blocker".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    let dependent = create(
        &conn,
        CreateItem {
            project_id: pid,
            state_id: sid,
            name: "Dependent".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
            sort_order: None,
            external_source: None,
            external_id: None,
            metadata: None,
            label_ids: vec![],
            assignee_ids: vec![],
            dependency_ids: vec![blocker.id.clone()],
            start_date: None,
            due_date: None,
        },
    )
    .unwrap();
    assert_eq!(
        dependents_of(&conn, &blocker.id).unwrap(),
        vec![dependent.id.clone()]
    );
    assert!(dependents_of(&conn, &dependent.id).unwrap().is_empty());
}

#[test]
fn all_dependencies_completed_requires_every_dependency_done() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let mk = |conn: &Connection, name: &str, deps: Vec<String>| {
        create(
            conn,
            CreateItem {
                project_id: pid.clone(),
                state_id: sid.clone(),
                name: name.into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: deps,
                start_date: None,
                due_date: None,
            },
        )
        .unwrap()
    };
    let dep_a = mk(&conn, "A", vec![]);
    let dep_b = mk(&conn, "B", vec![]);
    let dependent = mk(&conn, "Dependent", vec![dep_a.id.clone(), dep_b.id.clone()]);

    // Neither dependency is done yet.
    assert!(!all_dependencies_completed(&conn, &dependent.id).unwrap());

    let completed = crate::state::first_in_group(&conn, &pid, "completed").unwrap();
    update_state(&conn, &dep_a.id, &completed.id).unwrap();
    // dep_b is still open -- not all done.
    assert!(!all_dependencies_completed(&conn, &dependent.id).unwrap());

    update_state(&conn, &dep_b.id, &completed.id).unwrap();
    assert!(all_dependencies_completed(&conn, &dependent.id).unwrap());
}

#[test]
fn all_dependencies_completed_false_for_item_with_no_dependencies() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = create(
        &conn,
        CreateItem {
            project_id: pid,
            state_id: sid,
            name: "Solo".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    assert!(!all_dependencies_completed(&conn, &item.id).unwrap());
}

fn state_in_group(conn: &Connection, project_id: &str, group: &str) -> String {
    crate::state::list_by_project(conn, project_id)
        .unwrap()
        .into_iter()
        .find(|s| s.group_name == group)
        .unwrap()
        .id
}

#[test]
fn update_state_sets_started_at_when_moving_into_started_group() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid,
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    assert!(item.started_at.is_none());
    let started_state = state_in_group(&conn, &pid, "started");
    let updated = update_state(&conn, &item.id, &started_state).unwrap();
    assert!(updated.started_at.is_some());
    assert!(updated.completed_at.is_none());
}

#[test]
fn update_state_sets_completed_at_when_moving_into_completed_group() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid,
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    let completed_state = state_in_group(&conn, &pid, "completed");
    let updated = update_state(&conn, &item.id, &completed_state).unwrap();
    assert!(updated.completed_at.is_some());
}

#[test]
fn update_state_leaves_timestamps_none_when_moving_into_backlog() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid,
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    let backlog_state = state_in_group(&conn, &pid, "backlog");
    let updated = update_state(&conn, &item.id, &backlog_state).unwrap();
    assert!(updated.started_at.is_none());
    assert!(updated.completed_at.is_none());
}

#[test]
fn create_rejects_state_from_a_different_project() {
    let conn = db::open_in_memory().unwrap();
    let (pid1, _sid1) = seed_project(&conn, "1");
    let (_pid2, sid2) = seed_project(&conn, "2");
    assert!(matches!(
        create(
            &conn,
            CreateItem {
                project_id: pid1,
                state_id: sid2,
                name: "Test".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
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
        ),
        Err(crate::error::Error::InvalidTransition(_))
    ));
}

#[test]
fn update_state_rejects_state_from_a_different_project() {
    let conn = db::open_in_memory().unwrap();
    let (pid1, sid1) = seed_project(&conn, "1");
    let (pid2, _sid2) = seed_project(&conn, "2");
    let item = create(
        &conn,
        CreateItem {
            project_id: pid1,
            state_id: sid1,
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    let other_project_state = state_in_group(&conn, &pid2, "started");
    assert!(matches!(
        update_state(&conn, &item.id, &other_project_state),
        Err(crate::error::Error::InvalidTransition(_))
    ));
}

const TTL: i64 = 14400;

fn make_item(conn: &Connection, pid: &str, sid: &str) -> Item {
    create(
        conn,
        CreateItem {
            project_id: pid.to_string(),
            state_id: sid.to_string(),
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    .unwrap()
}

#[test]
fn claim_acquires_sets_assignee_and_moves_to_started_state() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    let outcome = claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    assert_eq!(outcome, ClaimOutcome::Acquired);
    let updated = get(&conn, &item.id).unwrap();
    assert_eq!(updated.assignee_agent.as_deref(), Some("agent:1"));
    assert_eq!(updated.state_id, state_in_group(&conn, &pid, "started"));
    assert!(updated.started_at.is_some());
}

#[test]
fn claim_on_already_held_item_returns_held_and_leaves_item_unchanged() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    let outcome = claim(&conn, &item.id, "agent:2", 1001, TTL).unwrap();
    assert!(matches!(
        outcome,
        ClaimOutcome::Held { ref owner, .. } if owner == "agent:1"
    ));
    let unchanged = get(&conn, &item.id).unwrap();
    assert_eq!(unchanged.assignee_agent.as_deref(), Some("agent:1"));
}

#[test]
fn release_clears_assignee_agent_so_a_different_agent_type_can_reclaim() {
    // item #93: a plain `claim::release` only drops the lease row, never
    // touching `assignee_agent` -- so a claimed-then-released item stayed
    // pinned to the released owner's agent type, and `claim()`'s
    // `BlockedByAssignee` guard (only exempts a same-agent-type reclaim)
    // refused every other agent type forever. `item::release` must clear
    // the pin so the item is genuinely available again.
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    assert_eq!(
        claim(&conn, &item.id, "claude-code:1", 1000, TTL).unwrap(),
        ClaimOutcome::Acquired
    );
    assert_eq!(
        get(&conn, &item.id).unwrap().assignee_agent.as_deref(),
        Some("claude-code:1")
    );

    assert!(release(&conn, &item.id, "claude-code:1").unwrap());
    assert_eq!(get(&conn, &item.id).unwrap().assignee_agent, None);

    let outcome = claim(&conn, &item.id, "opencode:1", 2000, TTL).unwrap();
    assert_eq!(outcome, ClaimOutcome::Acquired);
    assert_eq!(
        get(&conn, &item.id).unwrap().assignee_agent.as_deref(),
        Some("opencode:1")
    );
}

#[test]
fn release_with_nothing_to_release_leaves_assignee_agent_untouched() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    update(
        &conn,
        &item.id,
        UpdateItem {
            assignee_agent: Some("opencode".into()),
            ..Default::default()
        },
    )
    .unwrap();

    assert!(!release(&conn, &item.id, "claude-code:1").unwrap());
    assert_eq!(
        get(&conn, &item.id).unwrap().assignee_agent.as_deref(),
        Some("opencode")
    );
}

#[test]
fn create_rejects_due_date_before_start_date() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let err = create(
        &conn,
        CreateItem {
            project_id: pid,
            state_id: sid,
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
            sort_order: None,
            external_source: None,
            external_id: None,
            metadata: None,
            label_ids: vec![],
            assignee_ids: vec![],
            dependency_ids: vec![],
            start_date: Some(2000),
            due_date: Some(1000),
        },
    )
    .unwrap_err();
    assert!(matches!(err, crate::error::Error::Validation(_)), "{err}");
}

#[test]
fn update_rejects_due_date_before_the_items_existing_start_date() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    update(
        &conn,
        &item.id,
        UpdateItem {
            start_date: Some(2000),
            ..Default::default()
        },
    )
    .unwrap();

    let err = update(
        &conn,
        &item.id,
        UpdateItem {
            due_date: Some(1000),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(matches!(err, crate::error::Error::Validation(_)), "{err}");
}

#[test]
fn update_sets_start_date_and_due_date() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    let updated = update(
        &conn,
        &item.id,
        UpdateItem {
            start_date: Some(1000),
            due_date: Some(2000),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(updated.start_date, Some(1000));
    assert_eq!(updated.due_date, Some(2000));
}

#[test]
fn clear_item_start_date_nulls_only_start_date() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    update(
        &conn,
        &item.id,
        UpdateItem {
            start_date: Some(1000),
            due_date: Some(2000),
            ..Default::default()
        },
    )
    .unwrap();

    let cleared = clear_item_start_date(&conn, &item.id).unwrap();
    assert_eq!(cleared.start_date, None);
    assert_eq!(cleared.due_date, Some(2000));
}

#[test]
fn clear_item_due_date_nulls_only_due_date() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    update(
        &conn,
        &item.id,
        UpdateItem {
            start_date: Some(1000),
            due_date: Some(2000),
            ..Default::default()
        },
    )
    .unwrap();

    let cleared = clear_item_due_date(&conn, &item.id).unwrap();
    assert_eq!(cleared.due_date, None);
    assert_eq!(cleared.start_date, Some(1000));
}

#[test]
fn stale_claim_is_stealable_by_a_different_owner() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    let outcome = claim(&conn, &item.id, "agent:2", 1000 + TTL + 1, TTL).unwrap();
    assert_eq!(outcome, ClaimOutcome::Acquired);
    let updated = get(&conn, &item.id).unwrap();
    assert_eq!(updated.assignee_agent.as_deref(), Some("agent:2"));
}

#[test]
fn claim_by_a_different_agent_than_the_handoff_assignee_is_blocked() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    // Simulate a handoff: assignee set, never claimed yet.
    update(
        &conn,
        &item.id,
        UpdateItem {
            assignee_agent: Some("opencode".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let outcome = claim(&conn, &item.id, "claude-code:1", 1000, TTL).unwrap();
    assert_eq!(
        outcome,
        ClaimOutcome::BlockedByAssignee {
            assignee: "opencode".to_string()
        }
    );
    let unchanged = get(&conn, &item.id).unwrap();
    assert_eq!(unchanged.assignee_agent.as_deref(), Some("opencode"));
    assert!(crate::claim::current_owner(&conn, &item.id).is_none());
}

#[test]
fn claim_by_the_handoff_assignee_itself_succeeds() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    update(
        &conn,
        &item.id,
        UpdateItem {
            assignee_agent: Some("opencode".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let outcome = claim(&conn, &item.id, "opencode:1", 1000, TTL).unwrap();
    assert_eq!(outcome, ClaimOutcome::Acquired);
}

#[test]
fn claim_by_the_handoff_assignee_via_an_alias_succeeds() {
    // assignee_agent is canonicalized on write ("claude" -> "claude-code"),
    // but `owner` is the raw caller-supplied id — an alias owner must
    // still be recognized as the assignee, not blocked as an impostor.
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    update(
        &conn,
        &item.id,
        UpdateItem {
            assignee_agent: Some("claude".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        get(&conn, &item.id).unwrap().assignee_agent.as_deref(),
        Some("claude-code")
    );
    let outcome = claim(&conn, &item.id, "claude:1", 1000, TTL).unwrap();
    assert_eq!(outcome, ClaimOutcome::Acquired);
}

#[test]
fn current_owner_returns_the_claim_owner() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    assert!(crate::claim::current_owner(&conn, &item.id).is_none());
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    assert_eq!(
        crate::claim::current_owner(&conn, &item.id).as_deref(),
        Some("agent:1")
    );
}

#[test]
fn current_owner_returns_none_after_done() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    crate::claim::done(&conn, &item.id, "agent:1", 2000).unwrap();
    assert!(crate::claim::current_owner(&conn, &item.id).is_none());
}

#[test]
fn mark_completed_moves_to_completed_state_and_lease_stays_held() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    assert!(mark_completed(&conn, &item.id, "agent:1").unwrap());
    let done_item = get(&conn, &item.id).unwrap();
    assert_eq!(done_item.state_id, state_in_group(&conn, &pid, "completed"));
    assert!(done_item.completed_at.is_some());

    // Lease is still held — concurrent claim must be rejected.
    match claim(&conn, &item.id, "agent:2", 1200, TTL).unwrap() {
        ClaimOutcome::Held { .. } => {}
        other => panic!("expected Held after mark_completed, got {other:?}"),
    }

    // Release the lease, now re-acquirable.
    assert!(crate::claim::done(&conn, &item.id, "agent:1", 1300).unwrap());
    let outcome = claim(&conn, &item.id, "agent:2", 1400, TTL).unwrap();
    assert_eq!(outcome, ClaimOutcome::Acquired);
}

#[test]
fn mark_completed_noop_for_non_owner() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    assert!(!mark_completed(&conn, &item.id, "agent:2").unwrap());
}

#[test]
fn mark_in_review_moves_to_in_review_state_and_lease_stays_held() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    assert!(mark_in_review(&conn, &item.id, "agent:1").unwrap());
    let reviewed = get(&conn, &item.id).unwrap();
    assert_eq!(reviewed.state_id, state_in_group(&conn, &pid, "in_review"));
    // Not actually finished yet -- completed_at must stay unset.
    assert!(reviewed.completed_at.is_none());

    // Lease is still held, same contract as mark_completed.
    match claim(&conn, &item.id, "agent:2", 1200, TTL).unwrap() {
        ClaimOutcome::Held { .. } => {}
        other => panic!("expected Held after mark_in_review, got {other:?}"),
    }
}

#[test]
fn in_review_claim_past_the_default_ttl_is_reclaimable_even_under_the_full_ttl() {
    // Item #108: an item's job can finish (exit 0, PR opened, `mark_in_review`
    // called) and then sit un-heartbeated for longer than the short default
    // TTL while still well within the long active-work TTL a fresh claim
    // request asks for. Once in "in_review" there's no more concurrent work
    // to protect against, so a reclaim shouldn't have to wait out the full
    // active-work TTL just because nobody's heartbeat refreshed the lease.
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    assert!(mark_in_review(&conn, &item.id, "agent:1").unwrap());

    let default_ttl = crate::claim::default_ttl_secs();
    assert!(
        default_ttl < TTL,
        "test assumes the short default TTL is stricter than the long active-work TTL"
    );
    let now = 1000 + default_ttl + 1;
    // Still well within the full active-work TTL requested by the caller.
    assert!(now - 1000 < TTL);

    let outcome = claim(&conn, &item.id, "agent:2", now, TTL).unwrap();
    assert_eq!(outcome, ClaimOutcome::Acquired);
}

#[test]
fn mark_in_review_noop_for_non_owner() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    assert!(!mark_in_review(&conn, &item.id, "agent:2").unwrap());
}

#[test]
fn mark_in_review_backfills_the_state_for_a_project_seeded_before_it_existed() {
    // Simulates a project created before item #420: delete the
    // "in_review" state seed_defaults would otherwise have created, and
    // confirm mark_in_review heals it instead of erroring.
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let old_review_state_id = state_in_group(&conn, &pid, "in_review");
    conn.execute(
        "UPDATE states SET deleted_at = 1 WHERE id = ?1",
        rusqlite::params![old_review_state_id],
    )
    .unwrap();
    assert!(crate::state::first_in_group(&conn, &pid, "in_review").is_err());

    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    assert!(mark_in_review(&conn, &item.id, "agent:1").unwrap());

    let reviewed = get(&conn, &item.id).unwrap();
    let healed = crate::state::first_in_group(&conn, &pid, "in_review").unwrap();
    assert_eq!(reviewed.state_id, healed.id);
    assert_ne!(healed.id, old_review_state_id);
}

#[test]
fn promote_in_review_to_completed_moves_state_and_releases_the_lease() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    assert!(mark_in_review(&conn, &item.id, "agent:1").unwrap());

    assert!(promote_in_review_to_completed(&conn, &item.id).unwrap());
    let done_item = get(&conn, &item.id).unwrap();
    assert_eq!(done_item.state_id, state_in_group(&conn, &pid, "completed"));
    assert!(done_item.completed_at.is_some());

    // Lease was released -- a different agent can claim it now.
    let outcome = claim(&conn, &item.id, "agent:2", 1200, TTL).unwrap();
    assert_eq!(outcome, ClaimOutcome::Acquired);
}

#[test]
fn promote_in_review_to_completed_is_a_noop_when_not_in_review() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();
    // Still "started", never moved to in_review.
    assert!(!promote_in_review_to_completed(&conn, &item.id).unwrap());
    let unchanged = get(&conn, &item.id).unwrap();
    assert_eq!(unchanged.state_id, state_in_group(&conn, &pid, "started"));
}

#[test]
fn search_ranks_by_relevance() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid.clone(),
            name: "Database schema migration".into(),
            description: Some("Add users table".into()),
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid.clone(),
            name: "Fix login button".into(),
            description: Some("Update CSS for login page button".into()),
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid,
            name: "Backup database".into(),
            description: Some("PR-123 adds nightly DB backup".into()),
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    let results = search(&conn, &pid, "PR-123", None).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].description.contains("PR-123"));

    let db_results = search(&conn, &pid, "database", None).unwrap();
    assert_eq!(db_results.len(), 2);
    // Both matched — "Database" is in name of item 1, "database"
    // is in name of item 3. BM25 ranking may tie; verify both match.
    assert!(
        db_results[0].name.to_lowercase().contains("database")
            || db_results[0]
                .description
                .to_lowercase()
                .contains("database")
    );
}

#[test]
fn search_empty_query_returns_nothing() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid,
            name: "Test".into(),
            description: Some("something".into()),
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    let results = search(&conn, &pid, "", None).unwrap();
    assert!(results.is_empty());
}

#[test]
fn search_scoped_to_project() {
    let conn = db::open_in_memory().unwrap();
    let (pid1, sid1) = seed_project(&conn, "1");
    let (pid2, sid2) = seed_project(&conn, "2");
    create(
        &conn,
        CreateItem {
            project_id: pid1.clone(),
            state_id: sid1,
            name: "Database setup".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    create(
        &conn,
        CreateItem {
            project_id: pid2.clone(),
            state_id: sid2,
            name: "Database setup".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    assert_eq!(search(&conn, &pid1, "database", None).unwrap().len(), 1);
    assert_eq!(search(&conn, &pid2, "database", None).unwrap().len(), 1);
}

#[test]
fn search_falls_back_to_like_for_suffix_of_compound_token() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid,
            name: "Implement agentflare-store v1".into(),
            description: Some("unified local storage layer".into()),
            priority: None,
            parent_id: None,
            assignee_agent: None,
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

    // FTS5 tokenizes "agentflare-store" as ["agentflare", "store"], so a
    // bare "flare" query (a suffix, not a prefix, of "agentflare") finds
    // nothing via MATCH — only the LIKE fallback can find it.
    let results = search(&conn, &pid, "flare-store", None).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].name.contains("agentflare-store"));
}

#[test]
fn search_like_fallback_matches_literal_backslash_in_query() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid,
            name: r"agentflare\filter setup".into(),
            description: Some("unrelated".into()),
            priority: None,
            parent_id: None,
            assignee_agent: None,
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

    // FTS5 tokenizes on the backslash the same way it does on a hyphen
    // (see the suffix-of-compound-token test above), so "flare\filter"
    // has no whole-token FTS match and only the LIKE fallback can find
    // it. Before escaping backslashes first, `format!` left the query's
    // real `\` in the pattern un-doubled, so SQLite's `ESCAPE '\\'`
    // silently swallowed it as an (undefined) escape prefix for the
    // next character instead of matching it literally — the fallback
    // then missed a hit it should have found.
    let results = search(&conn, &pid, r"flare\filter", None).unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn heartbeat_release_done_are_owner_scoped() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "agent:1", 1000, TTL).unwrap();

    assert!(!crate::claim::heartbeat(&conn, &item.id, "agent:2", 1100).unwrap());
    assert!(!crate::claim::release(&conn, &item.id, "agent:2").unwrap());
    assert!(!crate::claim::done(&conn, &item.id, "agent:2", 1100).unwrap());

    assert!(crate::claim::heartbeat(&conn, &item.id, "agent:1", 1100).unwrap());
    assert!(crate::claim::done(&conn, &item.id, "agent:1", 1200).unwrap());
}

#[test]
fn resolve_id_passes_through_uuid_unchanged() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);

    let resolved = resolve_id(&conn, Some(&pid), &item.id).unwrap();
    assert_eq!(resolved, item.id);
}

#[test]
fn resolve_id_resolves_bare_numeric_sequence_id() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);

    let resolved = resolve_id(&conn, Some(&pid), &item.sequence_id.to_string()).unwrap();
    assert_eq!(resolved, item.id);
}

#[test]
fn resolve_id_resolves_hash_prefixed_sequence_id() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);

    let resolved = resolve_id(&conn, Some(&pid), &format!("#{}", item.sequence_id)).unwrap();
    assert_eq!(resolved, item.id);
}

#[test]
fn resolve_id_numeric_not_found_returns_not_found_error() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let _item = make_item(&conn, &pid, &sid);

    let err = resolve_id(&conn, Some(&pid), "999999").unwrap_err();
    assert!(matches!(err, crate::error::Error::NotFound(_)), "{err:?}");
}

#[test]
fn resolve_id_scopes_numeric_lookup_to_project() {
    let conn = db::open_in_memory().unwrap();
    let (pid_a, sid_a) = seed_project(&conn, "a");
    let (pid_b, _sid_b) = seed_project(&conn, "b");
    let item = make_item(&conn, &pid_a, &sid_a);

    // The item's sequence_id exists in project A but not project B.
    let err = resolve_id(&conn, Some(&pid_b), &item.sequence_id.to_string()).unwrap_err();
    assert!(matches!(err, crate::error::Error::NotFound(_)), "{err:?}");
}

#[test]
fn create_and_update_canonicalize_known_assignee_aliases() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");

    let item = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid.clone(),
            name: "Test".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: Some("claude".into()),
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
    assert_eq!(item.assignee_agent.as_deref(), Some("claude-code"));

    let updated = update(
        &conn,
        &item.id,
        UpdateItem {
            assignee_agent: Some("Claude Code".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(updated.assignee_agent.as_deref(), Some("claude-code"));
}

#[test]
fn list_by_label_returns_only_items_carrying_that_label() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "label");
    let ws_id = crate::project::get(&conn, &pid).unwrap().workspace_id;
    let label = crate::label::create(
        &conn,
        crate::label::CreateLabel {
            project_id: Some(pid.clone()),
            workspace_id: ws_id,
            name: "ready-for-work".into(),
            color: None,
            parent_id: None,
            sort_order: None,
            external_source: None,
            external_id: None,
        },
    )
    .unwrap();

    let labeled = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid.clone(),
            name: "Labeled".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid,
            name: "Unlabeled".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
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
    add_label(&conn, &labeled.id, &label.id).unwrap();

    let found = list_by_label(&conn, &pid, &label.id).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, labeled.id);
}

#[test]
fn update_sets_and_clears_parent_id() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let parent = make_item(&conn, &pid, &sid);
    let child = make_item(&conn, &pid, &sid);

    let reparented = update(
        &conn,
        &child.id,
        UpdateItem {
            parent_id: Some(Some(parent.id.clone())),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(reparented.parent_id.as_deref(), Some(parent.id.as_str()));
    assert_eq!(
        get(&conn, &child.id).unwrap().parent_id.as_deref(),
        Some(parent.id.as_str())
    );

    // An unrelated update must leave the parent alone...
    let renamed = update(
        &conn,
        &child.id,
        UpdateItem {
            name: Some("Renamed".into()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(renamed.parent_id.as_deref(), Some(parent.id.as_str()));

    // ...while an explicit Some(None) detaches it.
    let detached = update(
        &conn,
        &child.id,
        UpdateItem {
            parent_id: Some(None),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(detached.parent_id, None);
}

#[test]
fn update_rejects_self_parent() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    let err = update(
        &conn,
        &item.id,
        UpdateItem {
            parent_id: Some(Some(item.id.clone())),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, crate::error::Error::Validation(ref m) if m.contains("its own parent")),
        "{err:?}"
    );
    assert_eq!(get(&conn, &item.id).unwrap().parent_id, None);
}

#[test]
fn update_rejects_a_parent_that_would_close_a_cycle() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let grandparent = make_item(&conn, &pid, &sid);
    let parent = make_item(&conn, &pid, &sid);
    let child = make_item(&conn, &pid, &sid);
    for (item, new_parent) in [(&parent, &grandparent), (&child, &parent)] {
        update(
            &conn,
            &item.id,
            UpdateItem {
                parent_id: Some(Some(new_parent.id.clone())),
                ..Default::default()
            },
        )
        .unwrap();
    }

    // grandparent -> child would make the chain loop forever.
    let err = update(
        &conn,
        &grandparent.id,
        UpdateItem {
            parent_id: Some(Some(child.id.clone())),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, crate::error::Error::Validation(ref m) if m.contains("cycle")),
        "{err:?}"
    );
    assert_eq!(get(&conn, &grandparent.id).unwrap().parent_id, None);
}

#[test]
fn update_rejects_an_unknown_parent_instead_of_leaking_a_foreign_key_error() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    let err = update(
        &conn,
        &item.id,
        UpdateItem {
            parent_id: Some(Some("no-such-item".into())),
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(
        matches!(err, crate::error::Error::NotFound(ref m) if m.contains("no-such-item")),
        "{err:?}"
    );
}

fn make_label(conn: &Connection, pid: &str, name: &str) -> crate::label::Label {
    let ws_id = crate::project::get(conn, pid).unwrap().workspace_id;
    crate::label::create(
        conn,
        crate::label::CreateLabel {
            project_id: Some(pid.to_string()),
            workspace_id: ws_id,
            name: name.to_string(),
            color: None,
            parent_id: None,
            sort_order: None,
            external_source: None,
            external_id: None,
        },
    )
    .unwrap()
}

#[test]
fn redispatch_resets_state_clears_stale_labels_reattaches_ready_and_normalizes_assignee() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    let ready = make_label(&conn, &pid, "ready-for-work");
    let dispatched = make_label(&conn, &pid, "dispatched");
    let needs_manual = make_label(&conn, &pid, "needs-manual-dispatch");

    // Simulate the stuck post-failure shape: claimed (started state,
    // instance-suffixed assignee), left carrying the "dispatched" label
    // from the attempt that failed.
    claim(&conn, &item.id, "claude-code:abc123", 1000, TTL).unwrap();
    add_label(&conn, &item.id, &dispatched.id).unwrap();
    add_label(&conn, &item.id, &needs_manual.id).unwrap();

    let outcome = redispatch(&conn, &item.id, None).unwrap();
    assert_eq!(
        outcome,
        RedispatchOutcome::Ready {
            assignee_agent: "claude-code".to_string()
        }
    );

    let updated = get(&conn, &item.id).unwrap();
    assert_eq!(updated.state_id, state_in_group(&conn, &pid, "backlog"));
    assert_eq!(updated.assignee_agent.as_deref(), Some("claude-code"));

    let labels = list_labels(&conn, &item.id).unwrap();
    assert!(labels.contains(&ready.id));
    assert!(!labels.contains(&dispatched.id));
    assert!(!labels.contains(&needs_manual.id));
}

#[test]
fn redispatch_explicit_assignee_overrides_the_items_existing_one() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "opencode:1", 1000, TTL).unwrap();

    let outcome = redispatch(&conn, &item.id, Some("claude-code:override")).unwrap();
    assert_eq!(
        outcome,
        RedispatchOutcome::Ready {
            assignee_agent: "claude-code".to_string()
        }
    );
    assert_eq!(
        get(&conn, &item.id).unwrap().assignee_agent.as_deref(),
        Some("claude-code")
    );
}

#[test]
fn redispatch_to_a_different_agent_clears_a_model_scoped_to_the_old_one() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "opencode:1", 1000, TTL).unwrap();
    update(
        &conn,
        &item.id,
        UpdateItem {
            metadata: Some(r#"{"model":"opencode-go/deepseek-v4-pro","size":"M"}"#.to_string()),
            ..Default::default()
        },
    )
    .unwrap();

    let outcome = redispatch(&conn, &item.id, Some("claude-code:override")).unwrap();
    assert_eq!(
        outcome,
        RedispatchOutcome::Ready {
            assignee_agent: "claude-code".to_string()
        }
    );

    let updated = get(&conn, &item.id).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&updated.metadata).unwrap();
    assert!(
        metadata.get("model").is_none(),
        "model scoped to opencode must not survive a redispatch to claude-code: {metadata:?}"
    );
    assert_eq!(metadata.get("size").and_then(|v| v.as_str()), Some("M"));
}

#[test]
fn redispatch_to_the_same_agent_keeps_the_model_override() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "claude-code:1", 1000, TTL).unwrap();
    update(
        &conn,
        &item.id,
        UpdateItem {
            metadata: Some(r#"{"model":"claude-opus-4"}"#.to_string()),
            ..Default::default()
        },
    )
    .unwrap();

    redispatch(&conn, &item.id, None).unwrap();

    let updated = get(&conn, &item.id).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&updated.metadata).unwrap();
    assert_eq!(
        metadata.get("model").and_then(|v| v.as_str()),
        Some("claude-opus-4")
    );
}

#[test]
fn redispatch_with_no_assignee_anywhere_returns_no_assignee_and_makes_no_changes() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    let before = get(&conn, &item.id).unwrap();

    let outcome = redispatch(&conn, &item.id, None).unwrap();
    assert_eq!(outcome, RedispatchOutcome::NoAssignee);

    let after = get(&conn, &item.id).unwrap();
    assert_eq!(after.state_id, before.state_id);
    assert_eq!(after.assignee_agent, before.assignee_agent);
}

#[test]
fn redispatch_rejects_a_completed_item() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let item = make_item(&conn, &pid, &sid);
    claim(&conn, &item.id, "claude-code:1", 1000, TTL).unwrap();
    mark_completed(&conn, &item.id, "claude-code:1").unwrap();

    let err = redispatch(&conn, &item.id, Some("claude-code")).unwrap_err();
    assert!(
        matches!(err, crate::error::Error::Validation(ref m) if m.contains("completed")),
        "{err:?}"
    );
}

#[path = "tests_relations.rs"]
mod tests_relations;
