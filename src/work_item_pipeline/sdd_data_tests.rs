use super::*;

#[test]
fn work_item_data_roundtrips_sdd_fields() {
    let data = WorkItemData {
        tasks: vec![SddTask {
            id: 0,
            title: "Add config flag".to_string(),
            body: "Add --verbose flag to CLI".to_string(),
            model_tier: Some(TaskModelTier::Mechanical),
        }],
        current_task_index: 0,
        fix_round: 0,
        ledger: vec!["Task 0: dispatched".to_string()],
        last_report: None,
        ..Default::default()
    };
    let json = serde_json::to_string(&data).expect("serialize");
    let back: WorkItemData = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.tasks.len(), 1);
    assert_eq!(back.tasks[0].title, "Add config flag");
    assert_eq!(back.current_task_index, 0);
    assert_eq!(back.ledger, vec!["Task 0: dispatched".to_string()]);
}

#[test]
fn deserializes_persisted_state_json_from_before_review_only_existed() {
    // Runs started before item #507 persisted state_json with no
    // `review_only`/`review_findings` keys at all. Without `#[serde(default)]`
    // on those fields, SqliteStore::load fails to deserialize these rows and
    // recover() silently skips them as unreadable.
    let pre_507_json = r#"{
        "reply_text": "",
        "session_id": null,
        "cost_usd": null,
        "hold_reason": null,
        "review_issues": null,
        "pr_url": null,
        "tasks": [],
        "current_task_index": 0,
        "fix_round": 0,
        "ledger": [],
        "last_report": null
    }"#;
    let data: WorkItemData = serde_json::from_str(pre_507_json)
        .expect("old state_json without review_only must still deserialize");
    assert!(!data.review_only);
    assert!(data.review_findings.is_empty());
}
