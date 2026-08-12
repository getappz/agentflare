use super::*;

#[test]
fn item_update_state_by_name_lands_in_backlog() {
    let (tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let project_id = created["project_id"].as_str().unwrap().to_string();

    let updated: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(item_id),
            state_name: Some("backlog".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let backlog_id = {
        let conn = backend_conn(&tmp);
        agentflare_backend::state::list_by_project(&conn, &project_id)
            .unwrap()
            .into_iter()
            .find(|st| st.name == "Backlog")
            .unwrap()
            .id
    };
    assert_eq!(updated["state_id"], backlog_id);
}

#[test]
fn item_update_state_by_group_resolves_unambiguous_group() {
    let (tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let project_id = created["project_id"].as_str().unwrap().to_string();

    let updated: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(item_id),
            state_group: Some("started".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let started_id = {
        let conn = backend_conn(&tmp);
        agentflare_backend::state::list_by_project(&conn, &project_id)
            .unwrap()
            .into_iter()
            .find(|st| st.group_name == "started")
            .unwrap()
            .id
    };
    assert_eq!(updated["state_id"], started_id);
}

#[test]
fn item_update_state_rejects_unknown_name() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let err = s
        .item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(item_id),
            state_name: Some("Nonexistent State".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn item_update_state_rejects_ambiguous_group() {
    let (tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let project_id = created["project_id"].as_str().unwrap().to_string();

    {
        let conn = backend_conn(&tmp);
        agentflare_backend::state::create(
            &conn,
            agentflare_backend::state::CreateState {
                project_id: project_id.clone(),
                name: "Under Review".into(),
                group_name: "started".into(),
                sequence: 36000.0,
                is_default: None,
                color: None,
            },
        )
        .unwrap();
    }

    let err = s
        .item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(item_id),
            state_group: Some("started".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn item_update_state_rejects_state_id_and_state_name_together() {
    let (tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let project_id = created["project_id"].as_str().unwrap().to_string();

    let backlog_id = {
        let conn = backend_conn(&tmp);
        agentflare_backend::state::list_by_project(&conn, &project_id)
            .unwrap()
            .into_iter()
            .find(|st| st.name == "Backlog")
            .unwrap()
            .id
    };

    let err = s
        .item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(item_id),
            state_id: Some(backlog_id),
            state_name: Some("Backlog".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn item_update_state_rejects_missing_target() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let err = s
        .item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}
