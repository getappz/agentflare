use super::*;
use crate::error::Error;

fn make_epic(conn: &Connection, pid: &str, sid: &str, assignee: Option<&str>) -> Item {
    create(
        conn,
        CreateItem {
            project_id: pid.to_string(),
            state_id: sid.to_string(),
            name: "Epic".into(),
            description: None,
            priority: Some("high".into()),
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
    .unwrap()
}

const PLAN: &str = "# Plan\n\nGoal: ship\n\n\
### Task 1: Schema\n\nschema body\n\n```task\nsize: S\n```\n\n\
### Task 2: Parser\n\nparser body\n\n```task\nsize: M\ndepends_on: [1]\n```\n\n\
### Task 3: Wiring\n\nwiring body\n\n```task\nsize: L\ndepends_on: [1, 2]\n```\n";

fn children(conn: &Connection, epic_id: &str) -> Vec<Item> {
    let mut stmt = conn
        .prepare(
            "SELECT id FROM items WHERE parent_id = ?1 AND deleted_at IS NULL ORDER BY sequence_id",
        )
        .unwrap();
    let ids: Vec<String> = stmt
        .query_map([epic_id], |r| r.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    ids.iter().map(|id| get(conn, id).unwrap()).collect()
}

#[test]
fn creates_children_with_parent_edges_metadata_and_only_roots_ready() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let epic = make_epic(&conn, &pid, &sid, Some("claude-code"));

    let out = decompose_plan(&conn, &epic.id, PLAN).unwrap();
    assert_eq!((out.created, out.existing), (3, 0));
    assert!(out.warnings.is_empty());

    let kids = children(&conn, &epic.id);
    assert_eq!(kids.len(), 3);
    assert_eq!(kids[0].name, "Task 1: Schema");
    assert_eq!(kids[0].priority, "high");
    assert_eq!(kids[0].assignee_agent.as_deref(), Some("claude-code"));
    assert_eq!(kids[0].external_id.as_deref(), Some("task-1"));
    assert!(kids[0].description.contains("Goal: ship"));
    assert!(kids[0].description.contains("schema body"));
    let meta: serde_json::Value = serde_json::from_str(&kids[1].metadata).unwrap();
    assert_eq!(meta["size"], "M");
    assert_eq!(meta["plan_required"], false);

    assert!(list_dependencies(&conn, &kids[0].id).unwrap().is_empty());
    assert_eq!(
        list_dependencies(&conn, &kids[1].id).unwrap(),
        vec![kids[0].id.clone()]
    );
    assert_eq!(list_dependencies(&conn, &kids[2].id).unwrap().len(), 2);

    let ready = crate::label::get_by_name(&conn, &pid, READY_LABEL).unwrap();
    let ready_items = list_by_label(&conn, &pid, &ready.id).unwrap();
    assert_eq!(ready_items.len(), 1);
    assert_eq!(ready_items[0].id, kids[0].id);
}

#[test]
fn rerun_is_a_noop_and_never_rewrites_existing_children() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let epic = make_epic(&conn, &pid, &sid, Some("claude-code"));
    decompose_plan(&conn, &epic.id, PLAN).unwrap();

    let edited = PLAN.replace("Schema", "Renamed schema");
    let out = decompose_plan(&conn, &epic.id, &edited).unwrap();
    assert_eq!((out.created, out.existing), (0, 3));
    let kids = children(&conn, &epic.id);
    assert_eq!(kids.len(), 3);
    assert_eq!(kids[0].name, "Task 1: Schema");
}

#[test]
fn resumes_after_a_partial_run_and_wires_edges_to_the_existing_child() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let epic = make_epic(&conn, &pid, &sid, Some("claude-code"));
    // A previous interrupted run already created task 1.
    let pre = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid.clone(),
            name: "Task 1: Schema".into(),
            description: None,
            priority: None,
            parent_id: Some(epic.id.clone()),
            assignee_agent: None,
            sort_order: None,
            external_source: Some(plan_source(&epic.id)),
            external_id: Some("task-1".into()),
            metadata: None,
            label_ids: vec![],
            assignee_ids: vec![],
            dependency_ids: vec![],
            start_date: None,
            due_date: None,
        },
    )
    .unwrap();

    let out = decompose_plan(&conn, &epic.id, PLAN).unwrap();
    assert_eq!((out.created, out.existing), (2, 1));
    let kids = children(&conn, &epic.id);
    assert_eq!(kids.len(), 3);
    assert_eq!(list_dependencies(&conn, &kids[1].id).unwrap(), vec![pre.id]);
}

#[test]
fn invalid_plan_creates_nothing_and_reports_every_problem() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let epic = make_epic(&conn, &pid, &sid, Some("claude-code"));
    let bad = "### Task 1: A\n\n```task\nsize: m\n```\n\n### Task 2: B\n\n```task\nsize: S\ndepends_on: [9]\n```\n";

    let err = decompose_plan(&conn, &epic.id, bad).unwrap_err();
    match err {
        Error::Validation(msg) => {
            assert!(msg.contains("task 1"), "{msg}");
            assert!(msg.contains("depends_on 9"), "{msg}");
        }
        other => panic!("expected Validation, got {other:?}"),
    }
    assert!(children(&conn, &epic.id).is_empty());
}

#[test]
fn warns_when_the_epic_has_no_assignee() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let epic = make_epic(&conn, &pid, &sid, None);
    let out = decompose_plan(&conn, &epic.id, PLAN).unwrap();
    assert!(
        out.warnings.iter().any(|w| w.contains("assignee")),
        "{:?}",
        out.warnings
    );
}

fn finish(conn: &Connection, pid: &str, id: &str, group: &str) {
    let st = crate::state::first_in_group(conn, pid, group).unwrap();
    update_state(conn, id, &st.id).unwrap();
}

#[test]
fn epic_closes_only_when_every_child_is_finished() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let epic = make_epic(&conn, &pid, &sid, Some("claude-code"));
    decompose_plan(&conn, &epic.id, PLAN).unwrap();
    let kids = children(&conn, &epic.id);

    assert!(!close_epic_if_children_done(&conn, &epic.id).unwrap());
    finish(&conn, &pid, &kids[0].id, "completed");
    finish(&conn, &pid, &kids[1].id, "completed");
    assert!(!close_epic_if_children_done(&conn, &epic.id).unwrap());
    finish(&conn, &pid, &kids[2].id, "completed");

    assert!(close_epic_if_children_done(&conn, &epic.id).unwrap());
    let closed = get(&conn, &epic.id).unwrap();
    assert!(closed.completed_at.is_some());
    // Already closed: a second call reports nothing to do.
    assert!(!close_epic_if_children_done(&conn, &epic.id).unwrap());
}

#[test]
fn a_cancelled_child_counts_as_finished() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let epic = make_epic(&conn, &pid, &sid, Some("claude-code"));
    decompose_plan(&conn, &epic.id, PLAN).unwrap();
    let kids = children(&conn, &epic.id);
    finish(&conn, &pid, &kids[0].id, "completed");
    finish(&conn, &pid, &kids[1].id, "cancelled");
    finish(&conn, &pid, &kids[2].id, "completed");
    assert!(close_epic_if_children_done(&conn, &epic.id).unwrap());
}

#[test]
fn an_epic_without_plan_children_is_never_closed() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "");
    let epic = make_epic(&conn, &pid, &sid, None);
    assert!(!close_epic_if_children_done(&conn, &epic.id).unwrap());
}
