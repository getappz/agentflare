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

/// A plan-pending, human-approver item in the foreign project, seeded
/// straight into the db (the MCP `create`/`update` paths strip plan-transition
/// metadata, so this is how a foreign project's already-submitted plan looks).
fn make_pending_foreign_plan_item(conn: &rusqlite::Connection, pid: &str, sid: &str) -> String {
    let id = make_foreign_item(conn, pid, sid);
    agentflare_backend::item::update(
        conn,
        &id,
        agentflare_backend::item::UpdateItem {
            metadata: Some(
                r#"{"plan_required":true,"plan_approver":"human","plan_status":"pending"}"#.into(),
            ),
            ..Default::default()
        },
    )
    .unwrap();
    id
}

fn plan_status(tmp: &tempfile::TempDir, item_id: &str) -> serde_json::Value {
    let conn = backend_conn(tmp);
    let item = agentflare_backend::item::get(&conn, item_id).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
    metadata["plan_status"].clone()
}

fn plan_req(action: &str, id: &str) -> ItemRequest {
    ItemRequest {
        action: action.into(),
        id: Some(id.to_string()),
        ..Default::default()
    }
}

/// Bug: a Telegram "Approve" tap for an item in ANOTHER registered project
/// failed with `no item matches id '<uuid>'` because the channel route reused
/// the session-project-scoped `resolve_item_id`. The tap is human-authenticated
/// and its callback carries the item's own UUID, so the item's project wins --
/// but only on that route; every agent-callable route keeps the boundary.
#[test]
fn channel_approve_plan_reaches_an_item_in_another_project_only_via_channel() {
    crate::paths::test_support::with_temp_home(|| {
        let (tmp, s) = harness();
        // Link the session to its own project first.
        s.item(Parameters(empty_item_create("home project item")))
            .unwrap();
        let conn = backend_conn(&tmp);
        let (fpid, fsid) = seed_foreign_project(&conn);
        let foreign_id = make_pending_foreign_plan_item(&conn, &fpid, &fsid);
        drop(conn);

        // Agent-callable routes stay project-scoped (existence not leaked).
        for err in [
            s.item(Parameters(plan_req("approve_plan", &foreign_id)))
                .unwrap_err(),
            s.item_reject_plan(plan_req("reject_plan", &foreign_id))
                .unwrap_err(),
            s.item(Parameters(plan_req("get", &foreign_id)))
                .unwrap_err(),
        ] {
            assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
            assert!(err.message.contains("no item matches"), "{err:?}");
        }
        assert_eq!(plan_status(&tmp, &foreign_id), "pending");

        // A nonexistent id still gets the same generic error on the channel route.
        let missing = "00000000-0000-0000-0000-000000000000";
        let err = s
            .item_approve_plan_via_channel(plan_req("approve_plan", missing))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("no item matches"), "{err:?}");

        // The human's channel tap reaches the foreign project's item.
        let approved: serde_json::Value = serde_json::from_str(
            &s.item_approve_plan_via_channel(plan_req("approve_plan", &foreign_id))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(approved["status"], "approved");
        assert_eq!(plan_status(&tmp, &foreign_id), "approved");
    });
}

/// A bare numeric `sequence_id` is only meaningful inside one project, so the
/// channel route must NOT search all projects for it: it keeps resolving
/// against the session's linked project. Here both projects have an item #1;
/// approving "1" must never touch the foreign project's pending plan.
#[test]
fn channel_approve_plan_numeric_seq_stays_scoped_to_the_linked_project() {
    crate::paths::test_support::with_temp_home(|| {
        let (tmp, s) = harness();
        s.item(Parameters(empty_item_create("home project item")))
            .unwrap();
        let conn = backend_conn(&tmp);
        let (fpid, fsid) = seed_foreign_project(&conn);
        let foreign_id = make_pending_foreign_plan_item(&conn, &fpid, &fsid);
        drop(conn);

        // Home item #1 has no pending plan, so this is a state-mismatch error.
        let err = s
            .item_approve_plan_via_channel(plan_req("approve_plan", "1"))
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert!(err.message.contains("plan_status"), "{err:?}");
        assert_eq!(plan_status(&tmp, &foreign_id), "pending");
    });
}
