use super::*;

#[tokio::test]
async fn memory_promote_dispatch_requires_run_before_opening_memory() {
    let (_tmp, server) = harness();
    let request: MemoryRequest = serde_json::from_value(serde_json::json!({
        "action":"promote", "title":"lesson", "content":"claim", "project":"p",
        "evidence_ids":[1],"confidence":0.8
    }))
    .unwrap();
    let error = server.memory_impl(request).await.unwrap_err();
    assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(error.message.contains("run_id"));
}

#[tokio::test]
async fn memory_promote_dispatch_reads_reports_without_nesting_a_runtime() {
    let (_tmp, server) = harness();
    let request: MemoryRequest = serde_json::from_value(serde_json::json!({
        "action":"promote", "title":"lesson", "content":"claim", "project":"p",
        "evidence_ids":[1],"confidence":0.8,
        "run_id":"00000000-0000-0000-0000-000000000000"
    }))
    .unwrap();
    let error = server.memory_impl(request).await.unwrap_err();
    assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn promotion_run_must_belong_to_the_requested_project() {
    let (_tmp, server) = harness();
    let (name, other, report) = server
        .with_backend_db(|conn| {
            let project = server.resolve_project(conn)?;
            let other = agentflare_backend::project::create(
                conn,
                agentflare_backend::project::CreateProject {
                    workspace_id: project.workspace_id.clone(),
                    name: "other".into(),
                    identifier: "OTHER".into(),
                    external_source: None,
                    external_id: None,
                },
            )
            .map_err(map_backend_err)?;
            let state = agentflare_backend::state::list_by_project(conn, &project.id)
                .map_err(map_backend_err)?
                .remove(0);
            let item = agentflare_backend::item::create(
                conn,
                serde_json::from_value(serde_json::json!({
                    "name":"evidence", "project_id":project.id, "state_id":state.id,
                    "label_ids":[], "assignee_ids":[], "dependency_ids":[]
                }))
                .unwrap(),
            )
            .map_err(map_backend_err)?;
            Ok::<_, ErrorData>((
                project.name,
                other.name,
                serde_json::json!({"work_item":{"item_id":item.id}}),
            ))
        })
        .unwrap()
        .unwrap();
    assert_eq!(server.promotion_project(&report, &name).unwrap(), name);
    assert!(
        server
            .promotion_project(&report, &other)
            .unwrap_err()
            .message
            .contains("different project")
    );
    assert!(
        server
            .promotion_project(&report, "missing-project")
            .is_err()
    );
    assert!(
        server
            .promotion_project(&serde_json::json!({}), &name)
            .is_err()
    );
    assert!(
        server
            .promotion_project(
                &serde_json::json!({"work_item":{"item_id":"fabricated"}}),
                &name
            )
            .is_err()
    );
}
