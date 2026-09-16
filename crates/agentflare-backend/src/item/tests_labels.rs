use super::*;

#[test]
fn list_by_label_excludes_completed_and_cancelled_items() {
    // item #225: this is `run_discovery_tick`'s dispatch candidate query —
    // a label left on a completed/cancelled item (e.g. `item_cancel` not
    // clearing `ready-for-work`) used to keep it dispatchable forever.
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "label2");
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

    let active = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: sid,
            name: "Active".into(),
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
    let cancelled = create(
        &conn,
        CreateItem {
            project_id: pid.clone(),
            state_id: crate::state::first_in_group(&conn, &pid, "backlog")
                .unwrap()
                .id,
            name: "Cancelled".into(),
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
    add_label(&conn, &active.id, &label.id).unwrap();
    add_label(&conn, &cancelled.id, &label.id).unwrap();

    let cancelled_state = crate::state::first_in_group(&conn, &pid, "cancelled").unwrap();
    update_state(&conn, &cancelled.id, &cancelled_state.id).unwrap();

    let found = list_by_label(&conn, &pid, &label.id).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, active.id);
}
