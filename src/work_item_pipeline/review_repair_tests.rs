use super::*;
use crate::supervisor::review_findings::{ReviewOutcome, thread_result};
use flare_workflow::store::InMemoryStore;
use flare_workflow::{WorkflowDefinition, WorkflowEngine, WorkflowId, WorkflowStatus};
use std::sync::Arc;

#[tokio::test]
async fn item724_finalize_parked_repairs_report_missing_threads_and_refund_only_noops() {
    for (owner, fixed) in [
        ("codex:repair-job", false),
        ("codex:repair-job", true),
        ("codex:old-job", false),
    ] {
        let (mcp, _backend, _repo, item_id, _project, _worktree) =
            crate::claims::with_owner_override(owner, || {
                crate::mcp_server::tests::mcp_with_claimed_item("Parked review repair")
            });
        let mcp = Arc::new(mcp);
        mcp.with_backend_db(|conn| {
            crate::mcp_server::merge_item_metadata(conn, &item_id, |meta| {
                meta.insert(
                    "coderabbit_repair_attempt".into(),
                    serde_json::json!({
                        "job_id": "repair-job", "threads": {"PRRT_1": 2, "PRRT_2": 3},
                        "outcomes": {}, "credited": false
                    }),
                );
                crate::supervisor::review_findings::set_thread_round(meta, "PRRT_1", 2);
                crate::supervisor::review_findings::set_thread_round(meta, "PRRT_2", 3);
            })
            .unwrap();
        })
        .unwrap();
        if fixed {
            crate::claims::with_owner_override(owner, || {
                mcp.item_review_result(ItemRequest {
                    action: "review_result".into(),
                    id: Some(item_id.clone()),
                    thread_id: Some("PRRT_1".into()),
                    outcome: Some("fixed".into()),
                    sha: Some("abcdef1234567".into()),
                    note: Some("already fixed".into()),
                    ..Default::default()
                })
                .unwrap();
            });
        }
        let workflow = WorkflowDefinition::new(WORKFLOW_ID, "parked repair")
            .add_step(build_finalize_step(mcp.clone()));
        let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
        engine.register_workflow(workflow).unwrap();
        // Retried finalization must not duplicate credits or overwrite a fixed result.
        for _ in 0..2 {
            let data = WorkItemData {
                item_id: item_id.clone(),
                owner: owner.into(),
                hold_reason: Some("Await corrected PR target; stop retrying".into()),
                ..Default::default()
            };
            let run = engine
                .start_workflow(WorkflowId::new(WORKFLOW_ID), data, String::new())
                .await
                .unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                let status = engine.get_status(run).await.unwrap();
                if status.status == WorkflowStatus::Completed {
                    break;
                }
                assert_ne!(status.status, WorkflowStatus::Failed, "{:?}", status.error);
                assert!(std::time::Instant::now() < deadline, "finalize timed out");
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            let item = mcp
                .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
                .unwrap();
            let meta = crate::mcp_server::metadata_object(&item.metadata);
            let credits = meta
                .get("coderabbit_repair_noop_credits")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            assert_eq!(credits, u64::from(owner == "codex:repair-job" && !fixed));
            for (thread, round) in [("PRRT_1", 2), ("PRRT_2", 3)] {
                let result = thread_result(&meta, thread);
                if owner == "codex:old-job" {
                    assert!(
                        result.is_none(),
                        "stale finalizer must not report for a newer job"
                    );
                } else {
                    let result = result.expect("every parked finding needs a replyable outcome");
                    assert_eq!(result.round, round);
                    if fixed && thread == "PRRT_1" {
                        assert_eq!(result.outcome, ReviewOutcome::Fixed);
                        assert_eq!(result.sha.as_deref(), Some("abcdef1234567"));
                    } else {
                        assert_eq!(result.outcome, ReviewOutcome::Skipped);
                        assert!(result.note.contains("Await corrected PR target"));
                    }
                }
            }
        }
    }
}
