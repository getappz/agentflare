use super::*;

#[test]
fn item_redispatch_accepts_keep_pr_and_preserves_review_metadata() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap();
    let metadata = serde_json::json!({
        "pr": {"number": 501, "branch": "task/501", "ready": true},
        "review_threads": {"7": {"round": 1}},
        "coderabbit_repair_announced_for": "abc",
        "model": "old-model"
    });
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(item_id.into()),
        assignee_agent: Some("opencode".into()),
        metadata: Some(metadata.clone()),
        ..Default::default()
    }))
    .unwrap();
    let request: ItemRequest = serde_json::from_value(serde_json::json!({
        "action": "redispatch", "id": item_id, "keep_pr": true, "assignee_agent": "claude-code"
    }))
    .unwrap();
    s.item(Parameters(request)).unwrap();
    let updated = s
        .with_backend_db(|conn| agentflare_backend::item::get(conn, item_id).unwrap())
        .unwrap();
    let actual: serde_json::Value = serde_json::from_str(&updated.metadata).unwrap();
    for key in ["pr", "review_threads", "coderabbit_repair_announced_for"] {
        assert_eq!(actual[key], metadata[key]);
    }
    assert!(actual.get("model").is_none());
}
