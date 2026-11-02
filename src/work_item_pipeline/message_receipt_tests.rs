use super::*;
use crate::messages::receipts;
use flare_workflow::WorkflowDefinition;

fn message_db(path: &std::path::Path) -> rusqlite::Connection {
    let c = rusqlite::Connection::open(path).unwrap();
    c.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
    crate::messages::migrate(&c).unwrap();
    c
}

fn enqueue(c: &rusqlite::Connection) {
    c.execute(
        "INSERT INTO agent_messages(from_key,to_key,to_address,body,created_at)
        VALUES('sender:one','agent:one','agent:one','keep this correction',1)",
        [],
    )
    .unwrap();
}

async fn saved_run(
    path: &std::path::Path,
) -> (
    WorkflowEngine<WorkItemData, SqliteStore<WorkItemData>>,
    flare_workflow::WorkflowRunId,
) {
    let eng = WorkflowEngine::with_store(SqliteStore::open_file(path).unwrap());
    let run = flare_workflow::WorkflowRunId::new();
    eng.state_store()
        .save(flare_workflow::WorkflowState::new(
            run,
            WorkflowId::new(WORKFLOW_ID),
            WorkItemData {
                owner: "agent:one".into(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    (eng, run)
}

fn forget_outbox(run: flare_workflow::WorkflowRunId) {
    message_outboxes().lock().unwrap().remove(&run);
}

#[tokio::test]
async fn review_regression_preserves_numeric_message_order() {
    let tmp = tempfile::tempdir().unwrap();
    let messages = tmp.path().join("messages.db");
    let workflow = tmp.path().join("workflow.db");
    let c = message_db(&messages);
    for _ in 0..10 {
        enqueue(&c);
    }
    let (eng, run) = saved_run(&workflow).await;
    prepare_message_delivery(&eng, run, "agent:one", None, &messages, true)
        .await
        .unwrap();
    let mut data = eng.get_status(run).await.unwrap().context.data;
    assert_eq!(data.pending_corrections.len(), 10);
    let initial = data.pending_corrections[8].contains(" id=9 ")
        && data.pending_corrections[9].contains(" id=10 ");
    data.pending_corrections.clear();
    eng.refresh_registered_data_patch(run, &mut data);
    let restored = data.pending_corrections[8].contains(" id=9 ")
        && data.pending_corrections[9].contains(" id=10 ");
    assert_eq!(
        (initial, restored),
        (true, true),
        "message 9 must precede 10 on delivery and restoration"
    );
    forget_outbox(run);
}

#[tokio::test]
async fn review_regression_isolates_a_failed_run_during_boot() {
    let tmp = tempfile::tempdir().unwrap();
    let messages = tmp.path().join("messages.db");
    let workflow = tmp.path().join("workflow.db");
    let c = message_db(&messages);
    enqueue(&c);
    let (eng, bad) = saved_run(&workflow).await;
    receipts::reserve_for_run(&c, &bad.to_string(), "agent:one", 50, 60).unwrap();
    eng.state_store()
        .update(bad, |state| {
            state.resume_identity = Some("incompatible".into())
        })
        .await
        .unwrap();
    let healthy = flare_workflow::WorkflowRunId::new();
    eng.state_store()
        .save(flare_workflow::WorkflowState::new(
            healthy,
            WorkflowId::new(WORKFLOW_ID),
            WorkItemData {
                owner: "agent:two".into(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    let unknown = flare_workflow::WorkflowRunId::new();
    eng.state_store()
        .save(flare_workflow::WorkflowState::new(
            unknown,
            WorkflowId::new("unregistered"),
            WorkItemData {
                owner: "agent:three".into(),
                ..Default::default()
            },
        ))
        .await
        .unwrap();
    eng.register_workflow(
        WorkflowDefinition::new(WORKFLOW_ID, "test isolation").add_step(StepDefinition::new(
            "noop",
            "noop",
            flare_workflow::noop_executor::<WorkItemData>(),
        )),
    )
    .unwrap();
    let resumed = recover_with_message_custody(&eng, &messages, |_| true)
        .await
        .expect("one bad run must not prevent healthy recovery");
    assert_eq!(resumed, vec![healthy]);
    assert!(!eng.is_driving(bad));
    assert!(!eng.is_driving(unknown));
    for _ in 0..500 {
        if eng.get_status(healthy).await.unwrap().status == WorkflowStatus::Completed {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(
        eng.get_status(healthy).await.unwrap().status,
        WorkflowStatus::Completed
    );
    assert!(
        eng.get_status(bad)
            .await
            .unwrap()
            .context
            .data
            .accepted_message_receipts
            .is_empty()
    );
    assert_eq!(
        receipts::list(&c, "agent:one", 10).unwrap()[0].state,
        "in_doubt"
    );
    forget_outbox(bad);
    forget_outbox(healthy);
    forget_outbox(unknown);
}

#[tokio::test]
async fn recovery_store_enumeration_failure_still_propagates() {
    let tmp = tempfile::tempdir().unwrap();
    let workflow = tmp.path().join("workflow.db");
    let (eng, _) = saved_run(&workflow).await;
    rusqlite::Connection::open(&workflow)
        .unwrap()
        .execute_batch("ALTER TABLE workflow_runs RENAME TO unavailable_runs")
        .unwrap();
    assert!(
        recover_with_message_custody(&eng, &tmp.path().join("messages.db"), |_| true)
            .await
            .is_err()
    );
}

#[test]
fn old_workflow_json_defaults_receipt_evidence() {
    let mut json = serde_json::to_value(WorkItemData::default()).unwrap();
    assert!(json.get("accepted_message_receipts").is_some());
    json.as_object_mut()
        .unwrap()
        .remove("accepted_message_receipts");
    json.as_object_mut()
        .unwrap()
        .remove("pending_message_receipts");
    let old: WorkItemData = serde_json::from_value(json).unwrap();
    assert!(old.accepted_message_receipts.is_empty());
    assert!(old.pending_message_receipts.is_empty());
}

#[tokio::test]
async fn restart_before_receiver_custody_delivers_same_receipt_once() {
    let tmp = tempfile::tempdir().unwrap();
    let messages = tmp.path().join("messages.db");
    let workflow = tmp.path().join("workflow.db");
    let c = message_db(&messages);
    enqueue(&c);
    let (eng, run) = saved_run(&workflow).await;
    let reservation = receipts::reserve_for_run(&c, &run.to_string(), "agent:one", 50, 60)
        .unwrap()
        .remove(0);
    drop(eng);
    forget_outbox(run);
    let eng =
        WorkflowEngine::with_store(SqliteStore::<WorkItemData>::open_file(&workflow).unwrap());
    prepare_message_delivery(&eng, run, "agent:one", None, &messages, false)
        .await
        .unwrap();
    prepare_message_delivery(&eng, run, "agent:one", None, &messages, false)
        .await
        .unwrap();
    let data = eng.get_status(run).await.unwrap().context.data;
    assert_eq!(data.pending_corrections.len(), 1);
    assert!(data.pending_corrections[0].contains("keep this correction"));
    assert!(
        data.accepted_message_receipts
            .contains(&reservation.receipt_id)
    );
    assert_eq!(
        receipts::list(&c, "agent:one", 10).unwrap()[0].state,
        "received"
    );
    forget_outbox(run);
}

#[tokio::test]
async fn restart_after_custody_before_confirmation_does_not_reinsert_consumed_text() {
    let tmp = tempfile::tempdir().unwrap();
    let messages = tmp.path().join("messages.db");
    let workflow = tmp.path().join("workflow.db");
    let c = message_db(&messages);
    enqueue(&c);
    let (eng, run) = saved_run(&workflow).await;
    let r = receipts::reserve_for_run(&c, &run.to_string(), "agent:one", 50, 60)
        .unwrap()
        .remove(0);
    // A committed receiver checkpoint after its text was consumed, before sender confirmation.
    eng.state_store()
        .update(run, |s| {
            s.context
                .data
                .accepted_message_receipts
                .insert(r.receipt_id.clone());
        })
        .await
        .unwrap();
    drop(eng);
    forget_outbox(run);
    let eng =
        WorkflowEngine::with_store(SqliteStore::<WorkItemData>::open_file(&workflow).unwrap());
    prepare_message_delivery(&eng, run, "agent:new", Some("new-target"), &messages, false)
        .await
        .unwrap();
    let data = eng.get_status(run).await.unwrap().context.data;
    assert!(data.pending_corrections.is_empty());
    assert_eq!(data.owner, "agent:new");
    assert_eq!(data.target_branch, "new-target");
    assert_eq!(data.accepted_message_receipts.len(), 1);
    assert_eq!(
        receipts::list(&c, "agent:one", 10).unwrap()[0].state,
        "received"
    );
    // The restored patch preserves evidence even over a context loaded before adoption.
    let mut stale = WorkItemData::default();
    eng.refresh_registered_data_patch(run, &mut stale);
    assert!(stale.accepted_message_receipts.contains(&r.receipt_id));
    assert!(stale.pending_corrections.is_empty());
    forget_outbox(run);
}

#[tokio::test]
async fn stale_context_restores_unconsumed_text_even_when_receipt_id_is_present() {
    let tmp = tempfile::tempdir().unwrap();
    let messages = tmp.path().join("messages.db");
    let workflow = tmp.path().join("workflow.db");
    let c = message_db(&messages);
    enqueue(&c);
    let (eng, run) = saved_run(&workflow).await;
    prepare_message_delivery(&eng, run, "agent:one", None, &messages, true)
        .await
        .unwrap();
    let mut stale = eng.get_status(run).await.unwrap().context.data;
    assert_eq!(stale.pending_corrections.len(), 1);
    stale.pending_corrections.clear();
    eng.refresh_registered_data_patch(run, &mut stale);
    assert_eq!(stale.pending_corrections.len(), 1);
    consume_agent_messages(run, &stale.pending_corrections);
    stale.pending_corrections.clear();
    eng.refresh_registered_data_patch(run, &mut stale);
    assert!(stale.pending_corrections.is_empty());
    assert_eq!(stale.accepted_message_receipts.len(), 1);
    forget_outbox(run);
}

#[tokio::test]
async fn missing_receiver_and_failed_patch_leave_visible_unknown_receipts() {
    let tmp = tempfile::tempdir().unwrap();
    let messages = tmp.path().join("messages.db");
    let workflow = tmp.path().join("workflow.db");
    let c = message_db(&messages);
    enqueue(&c);
    let (eng, run) = saved_run(&workflow).await;
    receipts::reserve_for_run(&c, &run.to_string(), "agent:one", 50, 60).unwrap();
    eng.state_store().delete(run).await.unwrap();
    assert!(
        prepare_message_delivery(&eng, run, "agent:one", None, &messages, false)
            .await
            .is_err()
    );
    assert_eq!(
        receipts::list(&c, "agent:one", 10).unwrap()[0].state,
        "in_doubt"
    );
    assert!(!crate::messages::has_undelivered(&c, "agent:one").unwrap());
    assert_eq!(crate::messages::requeue(&c, &[1]).unwrap(), 0);
    forget_outbox(run);
}

#[tokio::test]
async fn boot_reconciles_before_starting_a_run_without_a_waiter() {
    let tmp = tempfile::tempdir().unwrap();
    let messages = tmp.path().join("messages.db");
    let workflow = tmp.path().join("workflow.db");
    let c = message_db(&messages);
    enqueue(&c);
    let (eng, run) = saved_run(&workflow).await;
    receipts::reserve_for_run(&c, &run.to_string(), "agent:one", 50, 60).unwrap();
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = seen.clone();
    let step = StepDefinition::new(
        "observe",
        "observe",
        std::sync::Arc::new(FunctionStep::new(
            move |ctx: &mut WorkflowContext<WorkItemData>| {
                let observed = observed.clone();
                Box::pin(async move {
                    assert_eq!(ctx.data.accepted_message_receipts.len(), 1);
                    assert_eq!(ctx.data.pending_corrections.len(), 1);
                    observed.store(true, std::sync::atomic::Ordering::SeqCst);
                    Ok(StepResult::Success)
                })
            },
        )),
    );
    eng.register_workflow(WorkflowDefinition::new(WORKFLOW_ID, "test custody").add_step(step))
        .unwrap();
    let resumed = recover_with_message_custody(&eng, &messages, |_| true)
        .await
        .unwrap();
    assert_eq!(resumed, vec![run]);
    for _ in 0..500 {
        if seen.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(seen.load(std::sync::atomic::Ordering::SeqCst));
    forget_outbox(run);
}

#[tokio::test]
async fn failed_receiver_checkpoint_stays_unknown_and_retry_does_not_duplicate() {
    let tmp = tempfile::tempdir().unwrap();
    let messages = tmp.path().join("messages.db");
    let workflow = tmp.path().join("workflow.db");
    let c = message_db(&messages);
    enqueue(&c);
    let (eng, run) = saved_run(&workflow).await;
    let wf = rusqlite::Connection::open(&workflow).unwrap();
    wf.execute_batch(
        "CREATE TRIGGER reject_checkpoint BEFORE UPDATE ON workflow_runs
        WHEN json_extract(NEW.state_json, '$.context.data.accepted_message_receipts')
             <> json_extract(OLD.state_json, '$.context.data.accepted_message_receipts')
        BEGIN SELECT RAISE(ABORT, 'fixture checkpoint failure'); END;",
    )
    .unwrap();
    assert!(
        prepare_message_delivery(&eng, run, "agent:one", None, &messages, true)
            .await
            .is_err()
    );
    assert_eq!(
        receipts::list(&c, "agent:one", 10).unwrap()[0].state,
        "in_doubt"
    );
    wf.execute_batch("DROP TRIGGER reject_checkpoint").unwrap();
    prepare_message_delivery(&eng, run, "agent:one", None, &messages, true)
        .await
        .unwrap();
    let data = eng.get_status(run).await.unwrap().context.data;
    assert_eq!(data.pending_corrections.len(), 1);
    assert_eq!(data.accepted_message_receipts.len(), 1);
    assert_eq!(
        receipts::list(&c, "agent:one", 10).unwrap()[0].state,
        "received"
    );
    forget_outbox(run);
}

#[tokio::test]
async fn confirmation_failure_reopens_with_one_pending_correction() {
    let tmp = tempfile::tempdir().unwrap();
    let messages = tmp.path().join("messages.db");
    let workflow = tmp.path().join("workflow.db");
    let c = message_db(&messages);
    enqueue(&c);
    let (eng, run) = saved_run(&workflow).await;
    c.execute_batch(
        "CREATE TRIGGER reject_confirmation BEFORE UPDATE ON agent_message_receipts
        BEGIN SELECT RAISE(ABORT, 'fixture confirmation failure'); END;",
    )
    .unwrap();
    assert!(
        prepare_message_delivery(&eng, run, "agent:one", None, &messages, true)
            .await
            .is_err()
    );
    assert_eq!(
        eng.get_status(run)
            .await
            .unwrap()
            .context
            .data
            .accepted_message_receipts
            .len(),
        1
    );
    assert_eq!(
        receipts::list(&c, "agent:one", 10).unwrap()[0].state,
        "in_doubt"
    );
    drop(eng);
    forget_outbox(run);
    c.execute_batch("DROP TRIGGER reject_confirmation").unwrap();
    let eng =
        WorkflowEngine::with_store(SqliteStore::<WorkItemData>::open_file(&workflow).unwrap());
    prepare_message_delivery(&eng, run, "agent:one", None, &messages, false)
        .await
        .unwrap();
    let mut data = eng.get_status(run).await.unwrap().context.data;
    assert_eq!(data.pending_corrections.len(), 1);
    data.pending_corrections.clear();
    eng.refresh_registered_data_patch(run, &mut data);
    assert_eq!(
        data.pending_corrections.len(),
        1,
        "restart must restore still-pending receipt text"
    );
    assert_eq!(
        receipts::list(&c, "agent:one", 10).unwrap()[0].state,
        "received"
    );
    forget_outbox(run);
}

#[tokio::test]
async fn changed_batch_composition_skips_the_already_consumed_receipt() {
    let tmp = tempfile::tempdir().unwrap();
    let messages = tmp.path().join("messages.db");
    let workflow = tmp.path().join("workflow.db");
    let c = message_db(&messages);
    enqueue(&c);
    enqueue(&c);
    let (eng, run) = saved_run(&workflow).await;
    let batch = receipts::reserve_for_run(&c, &run.to_string(), "agent:one", 50, 60).unwrap();
    eng.state_store()
        .update(run, |s| {
            s.context
                .data
                .accepted_message_receipts
                .insert(batch[0].receipt_id.clone());
        })
        .await
        .unwrap();
    prepare_message_delivery(&eng, run, "agent:one", None, &messages, false)
        .await
        .unwrap();
    let data = eng.get_status(run).await.unwrap().context.data;
    assert_eq!(data.accepted_message_receipts.len(), 2);
    assert_eq!(data.pending_corrections.len(), 1);
    assert!(data.pending_corrections[0].contains("id=2"));
    assert!(!data.pending_corrections[0].contains("id=1 "));
    forget_outbox(run);
}

#[tokio::test]
async fn concurrent_waiters_register_one_receipt_and_old_owner_cannot_take_more() {
    let tmp = tempfile::tempdir().unwrap();
    let messages = tmp.path().join("messages.db");
    let workflow = tmp.path().join("workflow.db");
    let c = message_db(&messages);
    enqueue(&c);
    let (eng, run) = saved_run(&workflow).await;
    let (a, b) = tokio::join!(
        prepare_message_delivery(&eng, run, "agent:one", None, &messages, true),
        prepare_message_delivery(&eng, run, "agent:one", None, &messages, true),
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(
        eng.get_status(run)
            .await
            .unwrap()
            .context
            .data
            .pending_corrections
            .len(),
        1
    );
    prepare_message_delivery(&eng, run, "agent:new", Some("main"), &messages, false)
        .await
        .unwrap();
    enqueue(&c);
    assert!(
        prepare_message_delivery(&eng, run, "agent:one", None, &messages, true)
            .await
            .is_err()
    );
    assert_eq!(
        crate::messages::count_undelivered(&c, "agent:one").unwrap(),
        1
    );
    forget_outbox(run);
}

#[tokio::test]
async fn boot_checkpoint_failure_never_drives_the_run() {
    let tmp = tempfile::tempdir().unwrap();
    let messages = tmp.path().join("messages.db");
    let workflow = tmp.path().join("workflow.db");
    let c = message_db(&messages);
    enqueue(&c);
    let (eng, run) = saved_run(&workflow).await;
    receipts::reserve_for_run(&c, &run.to_string(), "agent:one", 50, 60).unwrap();
    eng.register_workflow(WorkflowDefinition::new(WORKFLOW_ID, "test").add_step(
        StepDefinition::new(
            "noop",
            "noop",
            flare_workflow::noop_executor::<WorkItemData>(),
        ),
    ))
    .unwrap();
    let wf = rusqlite::Connection::open(&workflow).unwrap();
    wf.execute_batch(
        "CREATE TRIGGER reject_checkpoint BEFORE UPDATE ON workflow_runs
        WHEN json_extract(NEW.state_json, '$.context.data.accepted_message_receipts')
             <> json_extract(OLD.state_json, '$.context.data.accepted_message_receipts')
        BEGIN SELECT RAISE(ABORT, 'fixture checkpoint failure'); END;",
    )
    .unwrap();
    let resumed = recover_with_message_custody(&eng, &messages, |_| true)
        .await
        .unwrap();
    assert!(resumed.is_empty());
    assert!(!eng.is_driving(run));
    assert!(eng.get_status(run).await.unwrap().lease.is_none());
    assert_eq!(
        receipts::list(&c, "agent:one", 10).unwrap()[0].state,
        "in_doubt"
    );
    forget_outbox(run);
}

pub(super) fn handoff_barrier(phase: &str, run: flare_workflow::WorkflowRunId) {
    use std::io::Write;
    if std::env::var("AF_RECEIPT_TEST_PHASE").ok().as_deref() != Some(phase)
        || std::env::var("AF_RECEIPT_TEST_RUN").ok().as_deref() != Some(run.to_string().as_str())
    {
        return;
    }
    let dir = std::path::PathBuf::from(std::env::var_os("AF_RECEIPT_TEST_DIR").unwrap())
        .canonicalize()
        .unwrap();
    assert!(dir.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    let mut ready = std::fs::File::create(dir.join("ready.tmp")).unwrap();
    ready.write_all(phase.as_bytes()).unwrap();
    ready.sync_all().unwrap();
    drop(ready);
    std::fs::rename(dir.join("ready.tmp"), dir.join("ready")).unwrap();
    loop {
        std::thread::park();
    }
}

#[test]
fn crash_fixture_child() {
    let Some(dir) = std::env::var_os("AF_RECEIPT_TEST_DIR") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir).canonicalize().unwrap();
    assert!(dir.starts_with(std::env::temp_dir().canonicalize().unwrap()));
    let run: flare_workflow::WorkflowRunId = std::env::var("AF_RECEIPT_TEST_RUN")
        .unwrap()
        .parse()
        .unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let eng = WorkflowEngine::with_store(
                SqliteStore::<WorkItemData>::open_file(&dir.join("workflow.db")).unwrap(),
            );
            eng.register_workflow(
                WorkflowDefinition::new(WORKFLOW_ID, "crash custody").add_step(
                    StepDefinition::new(
                        "noop",
                        "noop",
                        flare_workflow::noop_executor::<WorkItemData>(),
                    ),
                ),
            )
            .unwrap();
            receipts::reserve_for_run(
                &message_db(&dir.join("messages.db")),
                &run.to_string(),
                "agent:one",
                i64::MAX,
                crate::claims::now(),
            )
            .unwrap();
            recover_with_message_custody(&eng, &dir.join("messages.db"), |_| true)
                .await
                .unwrap();
        });
}

struct OwnedChild(std::process::Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn killed_process_reconciles_both_real_handoff_windows() {
    for phase in ["before_custody", "after_custody"] {
        let tmp = tempfile::tempdir().unwrap();
        let messages = tmp.path().join("messages.db");
        let workflow = tmp.path().join("workflow.db");
        let c = message_db(&messages);
        enqueue(&c);
        let (eng, run) = saved_run(&workflow).await;
        drop(eng);
        let mut child = OwnedChild(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "work_item_pipeline::message_receipt_tests::crash_fixture_child",
                    "--nocapture",
                ])
                .env("AF_RECEIPT_TEST_DIR", tmp.path())
                .env("AF_RECEIPT_TEST_PHASE", phase)
                .env("AF_RECEIPT_TEST_RUN", run.to_string())
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !tmp.path().join("ready").exists() {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "child exited before {phase} barrier"
            );
            assert!(std::time::Instant::now() < deadline, "timed out at {phase}");
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("ready")).unwrap(),
            phase
        );
        let contender =
            WorkflowEngine::with_store(SqliteStore::<WorkItemData>::open_file(&workflow).unwrap());
        contender
            .register_workflow(WorkflowDefinition::new(WORKFLOW_ID, "contender").add_step(
                StepDefinition::new(
                    "noop",
                    "noop",
                    flare_workflow::noop_executor::<WorkItemData>(),
                ),
            ))
            .unwrap();
        assert!(
            contender.get_status(run).await.unwrap().lease.is_some(),
            "custody preparation must already own the lease"
        );
        assert!(
            !contender.resume_run(run).await.unwrap(),
            "another live engine cannot drive during custody preparation"
        );
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        let eng =
            WorkflowEngine::with_store(SqliteStore::<WorkItemData>::open_file(&workflow).unwrap());
        let before = eng.get_status(run).await.unwrap().context.data;
        assert_eq!(
            before.accepted_message_receipts.len(),
            usize::from(phase == "after_custody")
        );
        assert_eq!(
            receipts::list(&c, "agent:one", 10).unwrap()[0].state,
            "in_doubt"
        );
        prepare_message_delivery(&eng, run, "agent:one", None, &messages, false)
            .await
            .unwrap();
        prepare_message_delivery(&eng, run, "agent:one", None, &messages, false)
            .await
            .unwrap();
        let after = eng.get_status(run).await.unwrap().context.data;
        assert_eq!(after.pending_corrections.len(), 1);
        assert_eq!(after.accepted_message_receipts.len(), 1);
        assert_eq!(
            receipts::list(&c, "agent:one", 10).unwrap()[0].state,
            "received"
        );
        forget_outbox(run);
    }
}
