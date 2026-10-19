use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use flare_workflow::engine::WorkflowEngine;
use flare_workflow::json::{JsonWorkflow, PipelineData, SendMessage, compile_workflow_in};
use flare_workflow::sqlite_store::SqliteStore;
use flare_workflow::{
    StartOnceResult, StateStore, WorkflowDefinition, WorkflowError, WorkflowId, WorkflowRunId,
    WorkflowState, WorkflowStatus,
};

fn workflow() -> JsonWorkflow {
    serde_json::from_value(serde_json::json!({
        "name": "same-name",
        "steps": [{"name": "first", "agent": "coder", "prompt": "original"}]
    }))
    .unwrap()
}

fn sender(calls: Arc<AtomicUsize>) -> SendMessage {
    Arc::new(move |inv| {
        calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok((inv.prompt, 0, 0)) })
    })
}

// Simulate a crash after a step's journal commit but before run settlement.
async fn saved_run(
    path: &std::path::Path,
    json: &JsonWorkflow,
    cwd: &std::path::Path,
) -> flare_workflow::WorkflowRunId {
    let engine = WorkflowEngine::with_store(SqliteStore::open_file(path).unwrap());
    let definition = compile_workflow_in(
        json,
        sender(Arc::new(AtomicUsize::new(0))),
        Some(cwd.to_path_buf()),
    )
    .unwrap();
    engine.register_workflow(definition).unwrap();
    let run = engine
        .start_workflow(WorkflowId::new(&json.name), PipelineData, "input".into())
        .await
        .unwrap();
    engine
        .wait_for_completion(run, "test", Duration::from_secs(5))
        .await
        .unwrap();
    // Wait for the original driver to release its local claim as well.
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine.is_driving(run) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    engine
        .state_store()
        .update(run, |state| {
            state.status = WorkflowStatus::Paused;
            state.lease = None;
        })
        .await
        .unwrap();
    run
}

#[tokio::test]
async fn changed_json_or_cwd_cannot_resume_or_modify_a_saved_run() {
    let original = workflow();
    let mut changes = Vec::new();
    let mut changed = original.clone();
    changed.steps[0].prompt = "different instructions".into();
    changes.push(changed);
    let mut changed = original.clone();
    changed.steps[0].model = Some("different-model".into());
    changes.push(changed);
    let mut changed = original.clone();
    changed.steps[0].args.push("--different-policy".into());
    changes.push(changed);
    let mut changed = original.clone();
    changed.steps[0].name = "different-step".into();
    changes.push(changed);
    changes.push(original.clone()); // Same definition, different repository directory.

    for (index, changed) in changes.iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let path = dir.path().join("workflow.db");
        let run = saved_run(&path, &original, dir.path()).await;
        let calls = Arc::new(AtomicUsize::new(0));
        let engine =
            WorkflowEngine::with_store(SqliteStore::<PipelineData>::open_file(&path).unwrap());
        let before = serde_json::to_value(engine.get_status(run).await.unwrap()).unwrap();
        let journal_before = engine.state_store().journal(run).await.unwrap();
        let cwd = if index == changes.len() - 1 {
            other.path()
        } else {
            dir.path()
        };
        engine
            .register_workflow(
                compile_workflow_in(changed, sender(calls.clone()), Some(cwd.into())).unwrap(),
            )
            .unwrap();

        assert!(
            matches!(engine.resume_run(run).await, Err(WorkflowError::ResumeIdentityMismatch(id)) if id == run),
            "changed case {index} resumed"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(!engine.is_driving(run));
        assert_eq!(
            serde_json::to_value(engine.get_status(run).await.unwrap()).unwrap(),
            before
        );
        assert_eq!(
            engine.state_store().journal(run).await.unwrap().len(),
            journal_before.len()
        );
    }
}

#[tokio::test]
async fn unchanged_typed_json_resumes_without_reexecuting_completed_steps() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workflow.db");
    let run = saved_run(&path, &workflow(), dir.path()).await;
    let reordered: JsonWorkflow = serde_json::from_str(
        r#"{ "steps": [{ "prompt": "original", "agent": "coder", "name": "first" }], "name": "same-name" }"#,
    )
    .unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let engine = WorkflowEngine::with_store(SqliteStore::<PipelineData>::open_file(&path).unwrap());
    engine
        .register_workflow(
            compile_workflow_in(&reordered, sender(calls.clone()), Some(dir.path().into()))
                .unwrap(),
        )
        .unwrap();
    assert!(engine.resume_workflow(run).await.unwrap());
    engine
        .wait_for_completion(run, "test", Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        engine.get_status(run).await.unwrap().output.as_deref(),
        Some("original")
    );
}

#[tokio::test]
async fn legacy_json_state_is_readable_but_cannot_be_resumed_without_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workflow.db");
    let run = saved_run(&path, &workflow(), dir.path()).await;
    let store = SqliteStore::<PipelineData>::open_file(&path).unwrap();
    let mut legacy = serde_json::to_value(store.load(run).await.unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("resume_identity");
    store
        .save(serde_json::from_value(legacy).unwrap())
        .await
        .unwrap();
    let before = serde_json::to_value(store.load(run).await.unwrap()).unwrap();
    let engine = WorkflowEngine::with_store(store);
    engine
        .register_workflow(
            compile_workflow_in(
                &workflow(),
                sender(Arc::new(AtomicUsize::new(0))),
                Some(dir.path().into()),
            )
            .unwrap(),
        )
        .unwrap();
    assert!(
        matches!(engine.resume_workflow(run).await, Err(WorkflowError::ResumeIdentityMismatch(id)) if id == run)
    );
    assert_eq!(
        serde_json::to_value(engine.get_status(run).await.unwrap()).unwrap(),
        before
    );
}

#[tokio::test]
async fn recovery_skips_incompatible_runs_and_recovers_compatible_runs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("workflow.db");
    let old = saved_run(&path, &workflow(), dir.path()).await;
    let mut current = workflow();
    current.steps[0].prompt = "updated".into();
    let compatible = saved_run(&path, &current, dir.path()).await;
    let store = SqliteStore::<PipelineData>::open_file(&path).unwrap();
    for run in [old, compatible] {
        store
            .update(run, |state| state.status = WorkflowStatus::Running)
            .await
            .unwrap();
    }
    let before = serde_json::to_value(store.load(old).await.unwrap()).unwrap();
    let engine = WorkflowEngine::with_store(store);
    let calls = Arc::new(AtomicUsize::new(0));
    engine
        .register_workflow(
            compile_workflow_in(&current, sender(calls.clone()), Some(dir.path().into())).unwrap(),
        )
        .unwrap();
    assert_eq!(engine.recover().await.unwrap(), vec![compatible]);
    engine
        .wait_for_completion(compatible, "test", Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(engine.get_status(old).await.unwrap()).unwrap(),
        before
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn explicit_rust_revisions_reject_changes_and_removal_but_legacy_still_resumes() {
    for registered in [None, Some("v2")] {
        let store = SqliteStore::<PipelineData>::open_memory().unwrap();
        let run = WorkflowRunId::new();
        let mut state = WorkflowState::new(run, WorkflowId::new("rust"), PipelineData);
        state.resume_identity = Some("v1".into());
        store.save(state).await.unwrap();
        let engine = WorkflowEngine::with_store(store);
        let mut definition = WorkflowDefinition::new("rust", "rust");
        if let Some(revision) = registered {
            definition = definition.with_resume_identity(revision);
        }
        engine.register_workflow(definition).unwrap();
        assert!(
            matches!(engine.resume_run(run).await, Err(WorkflowError::ResumeIdentityMismatch(id)) if id == run)
        );
        assert!(!engine.is_driving(run));
    }

    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let run = WorkflowRunId::new();
    store
        .save(WorkflowState::new(
            run,
            WorkflowId::new("legacy"),
            PipelineData,
        ))
        .await
        .unwrap();
    let engine = WorkflowEngine::with_store(store);
    engine
        .register_workflow(WorkflowDefinition::new("legacy", "legacy"))
        .unwrap();
    assert!(engine.resume_run(run).await.unwrap());
    engine
        .wait_for_completion(run, "test", Duration::from_secs(5))
        .await
        .unwrap();
}

#[tokio::test]
async fn start_once_persists_identity_and_duplicate_receipt_never_relaunches() {
    let engine = WorkflowEngine::with_store(SqliteStore::<PipelineData>::open_memory().unwrap());
    engine
        .register_workflow(WorkflowDefinition::new("once", "once").with_resume_identity("v1"))
        .unwrap();
    let created = engine
        .start_workflow_once_with_params(
            "key",
            WorkflowId::new("once"),
            PipelineData,
            "input".into(),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert!(created.was_created());
    let run = created.run_id();
    engine
        .wait_for_completion(run, "test", Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(
        engine
            .get_status(run)
            .await
            .unwrap()
            .resume_identity
            .as_deref(),
        Some("v1")
    );
    engine
        .register_workflow(WorkflowDefinition::new("once", "once").with_resume_identity("v2"))
        .unwrap();
    let duplicate = engine
        .start_workflow_once_with_params(
            "key",
            WorkflowId::new("once"),
            PipelineData,
            "changed".into(),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(duplicate, StartOnceResult::Existing(run));
    assert!(!engine.resume_run(run).await.unwrap()); // Terminal runs never relaunch.
}
