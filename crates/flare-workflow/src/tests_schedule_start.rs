//! Start-once, receipt, and orphan-cancel tests (item #366 PR-B: T16–T21,
//! T24, plus the terminal-callback and disposition matrix). Real SQLite
//! transactions throughout; barriers and polling, never wall-clock sleeps
//! for lease expiry (stored leases are backdated instead).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::OrphanCancelDisposition;
use crate::engine::WorkflowEngine;
use crate::json::{PipelineData, SendMessage, StepInvocation, compile_workflow};
use crate::sqlite_store::SqliteStore;
use crate::store::StateStore;
use crate::types::*;

/// A `WorkflowData` whose serialization always fails: faults the state write
/// inside `create_start_once` to prove the receipt/journal roll back with it.
#[derive(Debug, Clone)]
struct FailSerialize;

impl serde::Serialize for FailSerialize {
    fn serialize<S: serde::Serializer>(&self, _: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("boom"))
    }
}

impl<'de> serde::Deserialize<'de> for FailSerialize {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let _: () = serde::Deserialize::deserialize(d)?;
        Ok(FailSerialize)
    }
}

impl WorkflowData for FailSerialize {
    fn workflow_type() -> &'static str {
        "fail-serialize"
    }
}

fn counting_send(counter: Arc<AtomicUsize>) -> SendMessage {
    Arc::new(move |inv: StepInvocation| {
        let counter = Arc::clone(&counter);
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok((format!("got {}", inv.prompt), 0, 0))
        })
    })
}

/// Never resolves: parks the driver in a live step without finishing.
fn blocked_send() -> SendMessage {
    Arc::new(|_: StepInvocation| {
        Box::pin(async move {
            let never: (String, u64, u64) = std::future::pending().await;
            Ok(never)
        })
    })
}

fn one_step_def(send: SendMessage) -> crate::definition::WorkflowDefinition<PipelineData> {
    let json: crate::json::JsonWorkflow = serde_json::from_str(
        r#"{"name": "w", "steps": [{"name": "s", "agent": "a", "prompt": "hi"}]}"#,
    )
    .unwrap();
    let mut def = compile_workflow(&json, send).unwrap();
    def.id = WorkflowId::new("w");
    def
}

/// Poll the store until `run_id` is terminal (real async work settling, not
/// clock manipulation).
async fn wait_terminal(
    store: &SqliteStore<PipelineData>,
    run_id: WorkflowRunId,
) -> WorkflowState<PipelineData> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let state = store.load(run_id).await.unwrap();
        if state.status.is_terminal() {
            return state;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "run {run_id} never settled"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn run_count(store: &SqliteStore<PipelineData>) -> usize {
    store.list_all().await.unwrap().len()
}

/// T16: concurrent starts for one key commit one receipt/run/input journal
/// and one driver; losers get `Existing`.
#[tokio::test]
async fn concurrent_starts_for_one_key_start_once() {
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let engine = WorkflowEngine::with_store(store.clone());
    let counter = Arc::new(AtomicUsize::new(0));
    engine
        .register_workflow(one_step_def(counting_send(counter.clone())))
        .unwrap();

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let (engine, barrier) = (engine.clone(), Arc::clone(&barrier));
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            engine
                .start_workflow_once_with_params(
                    "k",
                    WorkflowId::new("w"),
                    PipelineData,
                    String::new(),
                    serde_json::Value::Null,
                )
                .await
                .unwrap()
        }));
    }
    let mut ids = Vec::new();
    for t in tasks {
        ids.push(t.await.unwrap().run_id());
    }
    assert_eq!(ids[0], ids[1], "both starts resolve the same run");
    assert_eq!(run_count(&store).await, 1);
    assert_eq!(store.lookup_start("k").await.unwrap(), Some(ids[0]));
    assert_eq!(store.journal(ids[0]).await.unwrap().len(), 1);

    wait_terminal(&store, ids[0]).await;
    // One driver executed the single step exactly once.
    assert_eq!(counter.load(Ordering::SeqCst), 1);
}

/// T17: faults at each of the three atomic writes roll back all three.
#[tokio::test]
async fn start_once_faults_roll_back_receipt_state_and_journal() {
    // State-write fault (unserializable data).
    {
        let store = SqliteStore::<FailSerialize>::open_memory().unwrap();
        let run = WorkflowRunId::new();
        let mut state = WorkflowState::new(run, WorkflowId::new("w"), FailSerialize);
        state.status = WorkflowStatus::Running;
        let err = store
            .create_start_once(
                "k-state".to_string(),
                state,
                JournalEntry::Input { value: vec![] },
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("serialize"), "got {err:?}");
        assert_eq!(store.lookup_start("k-state").await.unwrap(), None);
        assert!(matches!(
            store.load(run).await.unwrap_err(),
            WorkflowError::NotFound(_)
        ));
        assert_eq!(store.journal(run).await.unwrap().len(), 0);
    }
    // Receipt-write fault (run_id UNIQUE conflict with a foreign key).
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t17.db");
        let store = SqliteStore::<PipelineData>::open_file(&path).unwrap();
        let run = WorkflowRunId::new();
        let setup = rusqlite::Connection::open(&path).unwrap();
        setup
            .execute(
                "INSERT INTO workflow_start_receipts (start_key, run_id, created_at) \
                 VALUES ('other', ?1, 'now')",
                rusqlite::params![run.to_string()],
            )
            .unwrap();
        drop(setup);
        let mut state = WorkflowState::new(run, WorkflowId::new("w"), PipelineData);
        state.status = WorkflowStatus::Running;
        let err = store
            .create_start_once(
                "k-receipt".to_string(),
                state,
                JournalEntry::Input { value: vec![] },
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("receipt"), "got {err:?}");
        assert_eq!(store.lookup_start("k-receipt").await.unwrap(), None);
        assert!(matches!(
            store.load(run).await.unwrap_err(),
            WorkflowError::NotFound(_)
        ));
        assert_eq!(store.journal(run).await.unwrap().len(), 0);
    }
    // Journal-write fault: a trigger aborting journal inserts for this run.
    // Receipt + state commit first in the same tx, so this proves the whole
    // transaction rolls back (no receipt, no run row).
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t17j.db");
        let store = SqliteStore::<PipelineData>::open_file(&path).unwrap();
        let run = WorkflowRunId::new();
        let setup = rusqlite::Connection::open(&path).unwrap();
        setup
            .execute_batch(&format!(
                "CREATE TRIGGER fault_journal BEFORE INSERT ON journal \
                 WHEN NEW.run_id = '{}' BEGIN SELECT RAISE(ABORT, 'journal fault'); END;",
                run
            ))
            .unwrap();
        drop(setup);
        let mut state = WorkflowState::new(run, WorkflowId::new("w"), PipelineData);
        state.status = WorkflowStatus::Running;
        let err = store
            .create_start_once(
                "k-journal".to_string(),
                state,
                JournalEntry::Input { value: vec![] },
            )
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("journal") || err.to_string().contains("UNIQUE"),
            "got {err:?}"
        );
        assert_eq!(store.lookup_start("k-journal").await.unwrap(), None);
        assert!(matches!(
            store.load(run).await.unwrap_err(),
            WorkflowError::NotFound(_)
        ));
    }
}

/// T18 (engine half): a live first start blocks duplicates — a concurrent
/// second start returns `Existing` without launching another driver.
#[tokio::test]
async fn concurrent_duplicate_start_while_first_in_flight_returns_existing() {
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let engine = WorkflowEngine::with_store(store.clone());
    engine
        .register_workflow(one_step_def(blocked_send()))
        .unwrap();

    let first = engine
        .start_workflow_once_with_params(
            "k",
            WorkflowId::new("w"),
            PipelineData,
            String::new(),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert!(first.was_created());
    // Driver is parked in its step; the lease is fresh.
    let second = engine
        .start_workflow_once_with_params(
            "k",
            WorkflowId::new("w"),
            PipelineData,
            String::new(),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(second, StartOnceResult::Existing(first.run_id()));
    assert_eq!(run_count(&store).await, 1);
    assert_eq!(engine.active_workflow_count(), 1);
}

/// T19 (engine half): after a commit without a driver, the next start for
/// the key returns the receipt's run and launches nothing.
#[tokio::test]
async fn start_after_commit_without_driver_returns_existing_without_launch() {
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let engine = WorkflowEngine::with_store(store.clone());
    let counter = Arc::new(AtomicUsize::new(0));
    engine
        .register_workflow(one_step_def(counting_send(counter.clone())))
        .unwrap();

    // Simulate "crash after commit before driver launch": durable
    // receipt/run/journal, no driver.
    let run = WorkflowRunId::new();
    let mut state = WorkflowState::new(run, WorkflowId::new("w"), PipelineData);
    state.status = WorkflowStatus::Running;
    state.lease = Some(ExecutorLease {
        owner: "dead:1:x".into(),
        renewed_at: Utc::now(),
    });
    store
        .create_start_once(
            "k".to_string(),
            state,
            JournalEntry::Input { value: vec![] },
        )
        .await
        .unwrap();

    let again = engine
        .start_workflow_once_with_params(
            "k",
            WorkflowId::new("w"),
            PipelineData,
            String::new(),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(again, StartOnceResult::Existing(run));
    assert_eq!(counter.load(Ordering::SeqCst), 0, "no driver launched");
    assert_eq!(run_count(&store).await, 1);
}

/// T20: an evicted-then-looked-up key still resolves the receipt — no new
/// driver, receipt preserved.
#[tokio::test]
async fn evicted_run_still_resolves_receipt_without_new_driver() {
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let engine = WorkflowEngine::with_store(store.clone());
    let counter = Arc::new(AtomicUsize::new(0));
    engine
        .register_workflow(one_step_def(counting_send(counter.clone())))
        .unwrap();

    let created = engine
        .start_workflow_once_with_params(
            "k",
            WorkflowId::new("w"),
            PipelineData,
            String::new(),
            serde_json::Value::Null,
        )
        .await
        .unwrap()
        .run_id();
    wait_terminal(&store, created).await;
    // Evict through the real deletion path.
    store.delete(created).await.unwrap();
    assert!(matches!(
        store.load(created).await.unwrap_err(),
        WorkflowError::NotFound(_)
    ));

    let again = engine
        .start_workflow_once_with_params(
            "k",
            WorkflowId::new("w"),
            PipelineData,
            String::new(),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(again, StartOnceResult::Existing(created));
    assert_eq!(store.lookup_start("k").await.unwrap(), Some(created));
    assert_eq!(run_count(&store).await, 0, "no replacement run");
    assert_eq!(counter.load(Ordering::SeqCst), 1, "no new driver");
}

/// T21 (engine half): a committed-but-never-driven run with a stale lease is
/// interrupted by conditional cancel — exactly one run, never a replacement
/// start.
#[tokio::test]
async fn committed_undriven_run_with_stale_lease_is_interrupted_not_replaced() {
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let engine = WorkflowEngine::with_store(store.clone());
    engine
        .register_workflow(one_step_def(blocked_send()))
        .unwrap();

    let run = WorkflowRunId::new();
    let mut state = WorkflowState::new(run, WorkflowId::new("w"), PipelineData);
    state.status = WorkflowStatus::Running;
    state.lease = Some(ExecutorLease {
        owner: "dead:1:x".into(),
        renewed_at: Utc::now() - chrono::Duration::hours(1),
    });
    store
        .create_start_once(
            "k".to_string(),
            state,
            JournalEntry::Input { value: vec![] },
        )
        .await
        .unwrap();

    let disp = engine.cancel_if_orphaned(run, Utc::now()).await.unwrap();
    assert_eq!(disp, OrphanCancelDisposition::Interrupted);
    assert_eq!(
        store.load(run).await.unwrap().status,
        WorkflowStatus::Cancelled
    );
    assert_eq!(run_count(&store).await, 1);
}

/// T24: post-commit eviction pressure cannot turn a committed start into an
/// apparent failure — `Created` still resolves, launches, and completes.
#[tokio::test]
async fn post_commit_eviction_pressure_still_yields_created_and_completion() {
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let engine = WorkflowEngine::with_store(store.clone());
    let counter = Arc::new(AtomicUsize::new(0));
    engine
        .register_workflow(one_step_def(counting_send(counter.clone())))
        .unwrap();

    // Push the store past the 200-run eviction cap with terminal runs.
    for _ in 0..205 {
        let run = WorkflowRunId::new();
        let mut s = WorkflowState::new(run, WorkflowId::new("w"), PipelineData);
        s.status = WorkflowStatus::Completed;
        store.save(s).await.unwrap();
    }
    let created = engine
        .start_workflow_once_with_params(
            "k",
            WorkflowId::new("w"),
            PipelineData,
            String::new(),
            serde_json::Value::Null,
        )
        .await
        .unwrap();
    assert!(created.was_created());
    let run_id = created.run_id();
    assert_eq!(store.lookup_start("k").await.unwrap(), Some(run_id));
    wait_terminal(&store, run_id).await;
    assert_eq!(counter.load(Ordering::SeqCst), 1);
    assert!(run_count(&store).await <= 201);
}

/// Terminal callback: exactly one `Completed` notification per settled run,
/// and a `Cancelled` one on real cancellation.
#[tokio::test]
async fn terminal_callback_fires_once_per_settlement() {
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let engine = WorkflowEngine::with_store(store.clone());
    let counter = Arc::new(AtomicUsize::new(0));
    engine
        .register_workflow(one_step_def(counting_send(counter.clone())))
        .unwrap();
    let seen: Arc<std::sync::Mutex<Vec<(WorkflowRunId, WorkflowStatus)>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_cb = Arc::clone(&seen);
    engine.on_terminal(Arc::new(move |run_id, status, _| {
        let seen_cb = Arc::clone(&seen_cb);
        Box::pin(async move {
            seen_cb.lock().unwrap().push((run_id, status));
        })
    }));

    let run = engine
        .start_workflow_once_with_params(
            "k1",
            WorkflowId::new("w"),
            PipelineData,
            String::new(),
            serde_json::Value::Null,
        )
        .await
        .unwrap()
        .run_id();
    wait_terminal(&store, run).await;
    // Let the awaited callback land (it runs before the settle returns, but
    // assert without assuming scheduling).
    assert_eq!(
        *seen.lock().unwrap(),
        vec![(run, WorkflowStatus::Completed)]
    );

    let run2 = engine
        .start_workflow_once_with_params(
            "k2",
            WorkflowId::new("w"),
            PipelineData,
            String::new(),
            serde_json::Value::Null,
        )
        .await
        .unwrap()
        .run_id();
    engine.cancel_workflow(run2).await.unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            (run, WorkflowStatus::Completed),
            (run2, WorkflowStatus::Cancelled)
        ]
    );
}

/// `cancel_if_orphaned` disposition matrix: fresh leases and paused runs
/// survive; terminal runs report; missing rows report; only stale orphans
/// flip to `Cancelled`.
#[tokio::test]
async fn orphan_cancel_dispositions() {
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let engine = WorkflowEngine::with_store(store.clone());
    let now = Utc::now();
    let past = now - chrono::Duration::hours(1);

    async fn put(
        store: &SqliteStore<PipelineData>,
        status: WorkflowStatus,
        lease: Option<ExecutorLease>,
    ) -> WorkflowRunId {
        let run = WorkflowRunId::new();
        let mut s = WorkflowState::new(run, WorkflowId::new("w"), PipelineData);
        s.status = status;
        s.lease = lease;
        store.save(s).await.unwrap();
        run
    }
    let lease_at = |t: DateTime<Utc>| {
        Some(ExecutorLease {
            owner: "dead:1:x".into(),
            renewed_at: t,
        })
    };

    // Stale lease on a live run: interrupted.
    let stale = put(&store, WorkflowStatus::Running, lease_at(past)).await;
    assert_eq!(
        engine.cancel_if_orphaned(stale, now).await.unwrap(),
        OrphanCancelDisposition::Interrupted
    );
    assert_eq!(
        store.load(stale).await.unwrap().status,
        WorkflowStatus::Cancelled
    );
    assert!(store.load(stale).await.unwrap().lease.is_none());

    // Fresh lease: untouched.
    let fresh = put(&store, WorkflowStatus::Running, lease_at(now)).await;
    assert_eq!(
        engine.cancel_if_orphaned(fresh, now).await.unwrap(),
        OrphanCancelDisposition::Unchanged
    );
    assert_eq!(
        store.load(fresh).await.unwrap().status,
        WorkflowStatus::Running
    );

    // Missing lease on a live run: interrupted.
    let no_lease = put(&store, WorkflowStatus::Waiting, None).await;
    assert_eq!(
        engine.cancel_if_orphaned(no_lease, now).await.unwrap(),
        OrphanCancelDisposition::Interrupted
    );

    // Paused with a stale lease: intentional inactivity, never cancelled.
    let paused = put(&store, WorkflowStatus::Paused, lease_at(past)).await;
    assert_eq!(
        engine.cancel_if_orphaned(paused, now).await.unwrap(),
        OrphanCancelDisposition::Unchanged
    );
    assert_eq!(
        store.load(paused).await.unwrap().status,
        WorkflowStatus::Paused
    );

    // Already terminal: real outcome reported, row untouched.
    let done = put(&store, WorkflowStatus::Failed, lease_at(past)).await;
    assert_eq!(
        engine.cancel_if_orphaned(done, now).await.unwrap(),
        OrphanCancelDisposition::Terminal(WorkflowStatus::Failed)
    );

    // Missing row.
    assert_eq!(
        engine
            .cancel_if_orphaned(WorkflowRunId::new(), now)
            .await
            .unwrap(),
        OrphanCancelDisposition::Missing
    );
}

/// Engine eviction/deletion paths must never delete start receipts.
#[tokio::test]
async fn deletion_never_removes_start_receipts() {
    let store = SqliteStore::<PipelineData>::open_memory().unwrap();
    let run = WorkflowRunId::new();
    let mut state = WorkflowState::new(run, WorkflowId::new("w"), PipelineData);
    state.status = WorkflowStatus::Running;
    store
        .create_start_once(
            "k".to_string(),
            state,
            JournalEntry::Input { value: vec![] },
        )
        .await
        .unwrap();
    store.delete(run).await.unwrap();
    assert_eq!(store.lookup_start("k").await.unwrap(), Some(run));
}
