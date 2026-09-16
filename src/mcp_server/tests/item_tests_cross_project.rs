use super::*;

/// Directly seeds a second project in the harness's own backend db, bypassing
/// `resolve_project` (which always resolves to the harness's one auto-linked
/// project) — mirrors `agentflare_backend::item::tests::seed_project`, the
/// backend-layer precedent for this same cross-project-rejection test shape.
fn seed_foreign_project(conn: &rusqlite::Connection) -> (String, String) {
    let ws = agentflare_backend::workspace::create(
        conn,
        agentflare_backend::workspace::CreateWorkspace {
            name: "Foreign".into(),
            slug: "foreign".into(),
            owner_agent: None,
            item_label: None,
        },
    )
    .unwrap();
    let proj = agentflare_backend::project::create(
        conn,
        agentflare_backend::project::CreateProject {
            workspace_id: ws.id.clone(),
            name: "Foreign".into(),
            identifier: "FOREIGN".into(),
            external_source: None,
            external_id: None,
        },
    )
    .unwrap();
    let state_id = agentflare_backend::state::list_by_project(conn, &proj.id)
        .unwrap()
        .into_iter()
        .find(|s| s.is_default)
        .unwrap()
        .id;
    (proj.id, state_id)
}

fn make_foreign_item(conn: &rusqlite::Connection, pid: &str, sid: &str) -> String {
    agentflare_backend::item::create(
        conn,
        agentflare_backend::item::CreateItem {
            project_id: pid.to_string(),
            state_id: sid.to_string(),
            name: "Foreign item".into(),
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
    .id
}

#[test]
fn item_get_rejects_id_from_another_project() {
    let (tmp, s) = harness();
    let conn = backend_conn(&tmp);
    let (fpid, fsid) = seed_foreign_project(&conn);
    let foreign_id = make_foreign_item(&conn, &fpid, &fsid);

    let err = s
        .item(Parameters(ItemRequest {
            action: "get".into(),
            id: Some(foreign_id.clone()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(
        err.message.contains("no item matches"),
        "must not leak that the id exists in a different project: {err:?}"
    );
}

#[test]
fn item_create_rejects_parent_id_from_another_project() {
    let (tmp, s) = harness();
    let conn = backend_conn(&tmp);
    let (fpid, fsid) = seed_foreign_project(&conn);
    let foreign_id = make_foreign_item(&conn, &fpid, &fsid);

    let err = s
        .item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Child".into()),
            parent_id: Some(foreign_id),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn comment_create_rejects_item_id_from_another_project() {
    let (tmp, s) = harness();
    let conn = backend_conn(&tmp);
    let (fpid, fsid) = seed_foreign_project(&conn);
    let foreign_id = make_foreign_item(&conn, &fpid, &fsid);

    let err = s
        .comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some(foreign_id),
            body: Some("hi".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}
