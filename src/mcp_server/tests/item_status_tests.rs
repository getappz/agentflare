use super::*;

#[test]
fn item_status_reports_defaults_for_a_never_dispatched_item() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let status: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "status".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(status["id"], item_id);
    assert_eq!(status["sequence_id"], 1);
    assert_eq!(status["name"], "Test");
    assert_eq!(status["state_group"], "backlog");
    assert!(status["job"].is_null(), "{status}");
    // No `job_queue_override` and a `backend_db_override` set (test harness)
    // -- `job_queue` deliberately skips the real on-disk queue, so `job` is
    // absent rather than erroring.
    assert_eq!(status["pr"]["status"], "unknown");
    assert_eq!(status["log_lines"], serde_json::json!([]));
}

#[test]
fn item_status_requires_id() {
    let (_tmp, s) = harness();
    let err = s
        .item(Parameters(ItemRequest {
            action: "status".into(),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn item_status_surfaces_the_most_recent_dispatch_job() {
    let (tmp, mut s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let queue = agentflare_jobs::Queue::open_memory(tmp.path().join("job-logs")).unwrap();
    // An older job for a *different* item must not be picked up.
    queue
        .enqueue(&agentflare_jobs::AgentJob::new("agentflare-work").args(["other-item", "agent:1"]))
        .unwrap();
    let dispatched = queue
        .enqueue(
            &agentflare_jobs::AgentJob::new("agentflare-work")
                .args([item_id.as_str(), "claude-code:1"])
                .in_process(),
        )
        .unwrap();
    s.job_queue_override = Some(queue);

    let status: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "status".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(status["job"]["id"], dispatched.id);
    assert_eq!(status["job"]["state"], "queued");
}

#[test]
fn item_status_log_lines_respects_the_limit_field() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    // No daemon log exists in this test environment -- limit is still
    // accepted and simply yields no lines, rather than erroring.
    let status: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "status".into(),
            id: Some(item_id),
            limit: Some(5),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(status["log_lines"], serde_json::json!([]));
}
