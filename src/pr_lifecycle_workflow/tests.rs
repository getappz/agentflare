use super::*;
use flare_workflow::{StartOnceResult, WorkflowDefinition, WorkflowEngine, WorkflowId};

#[test]
fn lifecycle_start_key_is_stable() {
    assert_eq!(lifecycle_start_key("p1", "i1", 99), "pr-lifecycle:p1:i1:99");
}

#[test]
fn review_sweep_engine_defaults_to_tick() {
    crate::paths::test_support::with_temp_home(|| {
        // SAFETY: with_temp_home holds GLOBAL_STATE_LOCK for process-wide env.
        unsafe {
            std::env::remove_var("AGENTFLARE_REVIEW_SWEEP_ENGINE");
            std::env::remove_var("AGENTFLARE_PR_LIFECYCLE_WORKFLOWS_REGISTER");
        }
        assert_eq!(review_sweep_engine(), ReviewSweepEngine::Tick);
        assert!(!should_register_at_boot());
    });
}

#[test]
fn env_flag_enables_boot_registration_without_workflow_engine() {
    crate::paths::test_support::with_temp_home(|| {
        unsafe {
            std::env::remove_var("AGENTFLARE_REVIEW_SWEEP_ENGINE");
            std::env::set_var("AGENTFLARE_PR_LIFECYCLE_WORKFLOWS_REGISTER", "1");
        }
        assert_eq!(review_sweep_engine(), ReviewSweepEngine::Tick);
        assert!(should_register_at_boot());
        unsafe {
            std::env::remove_var("AGENTFLARE_PR_LIFECYCLE_WORKFLOWS_REGISTER");
        }
    });
}

#[test]
fn review_sweep_engine_workflow_enables_boot_registration() {
    crate::paths::test_support::with_temp_home(|| {
        unsafe {
            std::env::set_var("AGENTFLARE_REVIEW_SWEEP_ENGINE", "workflow");
            std::env::remove_var("AGENTFLARE_PR_LIFECYCLE_WORKFLOWS_REGISTER");
        }
        assert_eq!(review_sweep_engine(), ReviewSweepEngine::Workflow);
        assert!(should_register_at_boot());
        unsafe {
            std::env::remove_var("AGENTFLARE_REVIEW_SWEEP_ENGINE");
        }
    });
}

#[test]
fn fan_out_calls_start_workflow_once_with_stable_keys() {
    crate::paths::test_support::with_temp_home(|| {
        let store = flare_workflow::SqliteStore::<PrLifecycleData>::open_memory().unwrap();
        let eng = WorkflowEngine::with_store(store.clone());
        let noop = std::sync::Arc::new(flare_workflow::executor::FunctionStep::new(
            |_ctx: &mut flare_workflow::WorkflowContext<PrLifecycleData>| {
                Box::pin(async { Ok(flare_workflow::StepResult::Success) })
            },
        ));
        let lifecycle = WorkflowDefinition::new(WORKFLOW_LIFECYCLE_ID, "pr lifecycle").add_step(
            StepDefinition::new("dispatch_status", "dispatch_status", noop),
        );
        eng.register_workflow(lifecycle).unwrap();

        let item = agentflare_backend::item::Item {
            id: "item-a".into(),
            project_id: "proj-a".into(),
            state_id: "state".into(),
            name: "n".into(),
            description: String::new(),
            priority: "medium".into(),
            parent_id: None,
            assignee_agent: None,
            sequence_id: 1,
            sort_order: 0.0,
            started_at: None,
            completed_at: None,
            archived_at: None,
            external_source: None,
            external_id: None,
            metadata: serde_json::json!({"pr": {"number": 7}}).to_string(),
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
            start_date: None,
            due_date: None,
        };
        let batches = vec![StoredReviewBatch {
            folder_path: "/tmp/repo".into(),
            project_id: "proj-a".into(),
            items: vec![item],
            label_id_by_name: Default::default(),
        }];
        let key = lifecycle_start_key("proj-a", "item-a", 7);
        crate::workflow::blocking_runtime().block_on(async {
            let first = fan_out_with_engine(&eng, &batches, "owner-1", serde_json::json!({}))
                .await
                .unwrap();
            assert_eq!(first, vec![key.clone()]);
            let second = fan_out_with_engine(&eng, &batches, "owner-1", serde_json::json!({}))
                .await
                .unwrap();
            assert_eq!(second, vec![key.clone()]);
            let run_id = store.lookup_start(&key).await.unwrap().unwrap();
            let again = eng
                .start_workflow_once_with_params(
                    &key,
                    WorkflowId::new(WORKFLOW_LIFECYCLE_ID),
                    PrLifecycleData::default(),
                    String::new(),
                    serde_json::json!({}),
                )
                .await
                .unwrap();
            assert_eq!(again, StartOnceResult::Existing(run_id));
        });
    });
}

/// Test seam: same fan-out as production but on an injected engine.
async fn fan_out_with_engine(
    eng: &WorkflowEngine<PrLifecycleData, flare_workflow::SqliteStore<PrLifecycleData>>,
    batches: &[StoredReviewBatch],
    discovery_owner: &str,
    params: serde_json::Value,
) -> WorkflowResult<Vec<String>> {
    let mut keys = Vec::new();
    let lifecycle_id = WorkflowId::new(WORKFLOW_LIFECYCLE_ID);
    for batch in batches {
        for item in &batch.items {
            let Some(number) = crate::worktree::pr_number_from_metadata(item) else {
                continue;
            };
            let key = lifecycle_start_key(&batch.project_id, &item.id, number);
            let data = PrLifecycleData {
                project_id: batch.project_id.clone(),
                folder_path: batch.folder_path.clone(),
                item: Some(item.clone()),
                pr_number: Some(number),
                label_id_by_name: batch.label_id_by_name.clone(),
                ..Default::default()
            };
            let mut run_params = params.clone();
            if let Some(obj) = run_params.as_object_mut() {
                obj.insert(
                    "discovery_owner".into(),
                    serde_json::Value::from(discovery_owner),
                );
            }
            eng.start_workflow_once_with_params(
                &key,
                lifecycle_id.clone(),
                data,
                String::new(),
                run_params,
            )
            .await?;
            keys.push(key);
        }
    }
    Ok(keys)
}

#[test]
fn engine_workflow_is_the_only_mode_without_the_sweep_loop() {
    // dispatch_boot spawns `spawn_supervisor_review_sweep` only for Tick.
    assert_eq!(
        parse_review_sweep_engine("workflow"),
        ReviewSweepEngine::Workflow
    );
    assert_eq!(parse_review_sweep_engine("tick"), ReviewSweepEngine::Tick);
    assert!(is_builtin_schedule_workflow(WORKFLOW_RECONCILE_ID));
    assert!(!is_builtin_schedule_workflow("release-check"));
}

#[tokio::test]
async fn sync_creates_one_skip_overlap_reconcile_schedule_per_repo_idempotently() {
    let dir = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let store = crate::schedule::ScheduleStore::open(&dir.path().join("wf.db")).unwrap();
    let roots = vec![repo.path().to_string_lossy().into_owned()];
    assert_eq!(sync_reconcile_schedules(&store, &roots).await, 1);
    assert_eq!(sync_reconcile_schedules(&store, &roots).await, 0);
    let rows = store.list_all().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].workflow_name, WORKFLOW_RECONCILE_ID);
    assert_eq!(rows[0].overlap, "skip");
    assert_eq!(rows[0].every_secs, crate::schedule::MIN_EVERY_SECS);
}

#[test]
fn clear_receipts_removes_only_that_items_lifecycle_keys() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("wf.db");
    let conn = rusqlite::Connection::open(&db).unwrap();
    conn.execute_batch(
        "CREATE TABLE workflow_start_receipts (start_key TEXT PRIMARY KEY, run_id TEXT, created_at INTEGER);
         INSERT INTO workflow_start_receipts VALUES ('pr-lifecycle:p:i1:5','r1',0),
           ('pr-lifecycle:p:i2:6','r2',0), ('schedule:x','r3',0);",
    )
    .unwrap();
    clear_receipts_in(&db, "p", "i1");
    let keys: Vec<String> = conn
        .prepare("SELECT start_key FROM workflow_start_receipts ORDER BY 1")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(keys, ["pr-lifecycle:p:i2:6", "schedule:x"]);
}
