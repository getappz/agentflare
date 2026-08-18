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
