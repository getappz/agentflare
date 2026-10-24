use super::*;

fn call(s: &AgentflareMcp, req: ItemRequest) -> serde_json::Value {
    serde_json::from_str(&s.item(Parameters(req)).unwrap()).unwrap()
}

#[test]
fn item_clear_parent_detaches_child() {
    let (_tmp, s) = harness();
    let parent = call(&s, empty_item_create("Parent"));
    let child = call(
        &s,
        ItemRequest {
            parent_id: Some(parent["id"].as_str().unwrap().into()),
            ..empty_item_create("Child")
        },
    );
    assert_eq!(child["parent_id"], parent["id"]);

    let cleared = call(
        &s,
        ItemRequest {
            action: "clear_parent".into(),
            id: Some(child["sequence_id"].to_string()),
            ..Default::default()
        },
    );
    assert_eq!(cleared["parent_id"], serde_json::Value::Null);
    let fetched = call(
        &s,
        ItemRequest {
            action: "get".into(),
            id: Some(child["id"].as_str().unwrap().into()),
            ..Default::default()
        },
    );
    assert_eq!(fetched["parent_id"], serde_json::Value::Null);
    assert_eq!(fetched["name"], child["name"]);
}

#[test]
fn item_clear_parent_on_root_item_is_noop() {
    let (_tmp, s) = harness();
    let root = call(&s, empty_item_create("Root"));
    let cleared = call(
        &s,
        ItemRequest {
            action: "clear_parent".into(),
            id: Some(root["id"].as_str().unwrap().into()),
            ..Default::default()
        },
    );
    assert_eq!(cleared["parent_id"], serde_json::Value::Null);
    assert_eq!(cleared["id"], root["id"]);
}

#[test]
fn item_clear_parent_without_id_names_its_own_action() {
    let (_tmp, s) = harness();
    let err = s
        .item(Parameters(ItemRequest {
            action: "clear_parent".into(),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(err.message.contains("clear_parent"), "{}", err.message);
}

#[test]
fn item_update_parent_id_whitespace_detaches() {
    let (_tmp, s) = harness();
    let parent = call(&s, empty_item_create("Parent"));
    let child = call(
        &s,
        ItemRequest {
            parent_id: Some(parent["id"].as_str().unwrap().into()),
            ..empty_item_create("Child")
        },
    );
    assert_eq!(child["parent_id"], parent["id"]);
    let id = child["id"].as_str().unwrap().to_string();
    let updated = call(
        &s,
        ItemRequest {
            action: "update".into(),
            id: Some(id.clone()),
            parent_id: Some("  ".into()),
            ..Default::default()
        },
    );
    assert_eq!(updated["parent_id"], serde_json::Value::Null);
    let fetched = call(
        &s,
        ItemRequest {
            action: "get".into(),
            id: Some(id),
            ..Default::default()
        },
    );
    assert_eq!(fetched["parent_id"], serde_json::Value::Null);
}
