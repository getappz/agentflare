//! Scheduler execution-context/lifecycle tests (item #366 PR-B: T36–T44,
//! T49, T49b). Command workflows execute for real (no agent binary needed);
//! agent steps run against capturing mock sends.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::Utc;
use flare_workflow::json::PipelineData;
use flare_workflow::types::*;
use flare_workflow::{StateStore, WorkflowRunId};

use super::store::{CreateInput, UpdateInput};
use super::tests::{Harness, capture_send, mock_send};
use super::{OccurrenceRow, start_key_for};

/// T36: execution lands in the schedule's repo — command cwd and agent cwd.
#[tokio::test]
async fn t36_execution_runs_in_schedule_repo() {
    let cwds = Arc::new(std::sync::Mutex::new(Vec::new()));
    let counter = Arc::new(AtomicUsize::new(0));
    let h = Harness::with_send(capture_send(Arc::clone(&counter), cwds.clone()));
    // No process-wide chdir anywhere: the daemon's ambient cwd is wherever
    // the test runner sits, and execution must still land in repo_b.
    let repo_b = h.repo("b");
    h.write_wf(
        &repo_b,
        "w",
        r#"{"name":"w","steps":[{"name":"s","command":["pwd"]}]}"#,
    );
    h.write_wf(
        &repo_b,
        "ag",
        r#"{"name":"ag","steps":[{"name":"s","agent":"a","prompt":"hi"}]}"#,
    );
    let t0 = Utc::now().timestamp();
    // Command step output is the schedule's repo, not the daemon's cwd.
    let s = h.mk(&repo_b, "s", "w", 300, t0).await;
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.tick(h.at(t0, 1)).await;
    let occ = h.wait_occ(&r.occurrence_id).await;
    assert_eq!(occ.status, "completed");
    let run_id: WorkflowRunId = occ.run_id.unwrap().parse().unwrap();
    let state = h.sched.engine.state_store().load(run_id).await.unwrap();
    let out = state.output.unwrap_or_default();
    assert_eq!(
        std::fs::canonicalize(out.trim()).unwrap(),
        std::fs::canonicalize(&repo_b).unwrap()
    );
    // Agent steps receive the same cwd in the invocation.
    let sa = h.mk(&repo_b, "sa", "ag", 300, t0).await;
    let ra = h.sched.store().enqueue_manual(&sa.id, t0).await.unwrap();
    h.tick(h.at(t0, 2)).await;
    h.wait_occ(&ra.occurrence_id).await;
    let expected = std::fs::canonicalize(&repo_b).unwrap();
    let got = cwds.lock().unwrap().clone();
    assert!(!got.is_empty());
    assert!(got.iter().all(|c| c.as_ref() == Some(&expected)));
    assert!(counter.load(Ordering::SeqCst) > 0);
}

/// T37: same workflow name in two repos executes each repo's own file;
/// concurrent same-schedule starts share one definition registration.
#[tokio::test]
async fn t37_repos_stay_isolated_and_concurrent_starts_share_def() {
    let h = Harness::new();
    let repo_a = h.repo("a");
    let repo_b = h.repo("b");
    h.echo_wf(&repo_a, "nightly", "OUT-A");
    h.echo_wf(&repo_b, "nightly", "OUT-B");
    let t0 = Utc::now().timestamp();
    let sa = h.mk(&repo_a, "sa", "nightly", 300, t0).await;
    let sb = h.mk(&repo_b, "nightly", "nightly", 300, t0).await;
    let ra = h.sched.store().enqueue_manual(&sa.id, t0).await.unwrap();
    let rb = h.sched.store().enqueue_manual(&sb.id, t0).await.unwrap();
    h.tick(h.at(t0, 1)).await;
    let (oa, ob) = (
        h.wait_occ(&ra.occurrence_id).await,
        h.wait_occ(&rb.occurrence_id).await,
    );
    assert_eq!(
        (oa.status.as_str(), ob.status.as_str()),
        ("completed", "completed")
    );
    let out = |o: &OccurrenceRow| {
        let run: WorkflowRunId = o.run_id.as_ref().unwrap().parse().unwrap();
        run
    };
    let sta = h.sched.engine.state_store().load(out(&oa)).await.unwrap();
    let stb = h.sched.engine.state_store().load(out(&ob)).await.unwrap();
    assert_eq!(sta.output.as_deref(), Some("OUT-A"));
    assert_eq!(stb.output.as_deref(), Some("OUT-B"));
    // Concurrent same-schedule starts: two occurrences, two runs, one output.
    h.sched
        .store()
        .update_schedule(
            &sa.id,
            UpdateInput {
                overlap: Some("allow".into()),
                ..Default::default()
            },
            t0,
        )
        .await
        .unwrap();
    let r1 = h.sched.store().enqueue_manual(&sa.id, t0).await.unwrap();
    let r2 = h.sched.store().enqueue_manual(&sa.id, t0).await.unwrap();
    h.tick(h.at(t0, 2)).await;
    let (o1, o2) = (
        h.wait_occ(&r1.occurrence_id).await,
        h.wait_occ(&r2.occurrence_id).await,
    );
    assert_eq!(
        (o1.status.as_str(), o2.status.as_str()),
        ("completed", "completed")
    );
    assert_ne!(o1.run_id, o2.run_id);
}

/// T38: missing/invalid workflow at execution → `dispatch_failed`, enabled.
#[tokio::test]
async fn t38_bad_definition_at_execution_fails_open() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    std::fs::remove_file(repo.join(".agentflare").join("workflows").join("w.json")).unwrap();
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.tick(h.at(t0, 1)).await;
    let occ = h.wait_occ(&r.occurrence_id).await;
    assert_eq!(occ.status, "dispatch_failed");
    assert!(
        h.sched
            .store()
            .get_by_id(&s.id)
            .await
            .unwrap()
            .unwrap()
            .enabled
    );
}

/// T39: params corruption freezes only that schedule; update clears it.
#[tokio::test]
async fn t39_corrupt_params_freeze_and_update_clears() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let bad = h.mk(&repo, "bad", "w", 300, t0).await;
    let good = h.mk(&repo, "good", "w", 300, t0).await;
    h.sched
        .store()
        .test_exec(format!(
            "UPDATE schedules SET params = '{{oops' WHERE id = '{}';",
            bad.id
        ))
        .await
        .unwrap();
    let summary = h.tick(h.at(t0, 310)).await;
    h.sched.drain_fires().await;
    assert_eq!(summary.admitted, 1);
    assert!(summary.errors.iter().any(|e| e.contains("frozen")));
    let bad_row = h.sched.store().get_by_id(&bad.id).await.unwrap().unwrap();
    assert!(bad_row.frozen_error.is_some());
    assert!(
        h.sched
            .store()
            .occurrences_for_schedule(&bad.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
    // Only that schedule froze.
    assert_eq!(
        h.sched
            .store()
            .occurrences_for_schedule(&good.id, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    // Update clears the freeze and admission resumes.
    h.sched
        .store()
        .update_schedule(
            &bad.id,
            UpdateInput {
                params: Some(serde_json::json!({})),
                ..Default::default()
            },
            t0 + 310,
        )
        .await
        .unwrap();
    let bad_row = h.sched.store().get_by_id(&bad.id).await.unwrap().unwrap();
    assert!(bad_row.frozen_error.is_none());
    let summary = h.tick(h.at(t0, 320)).await;
    h.sched.drain_fires().await;
    assert!(summary.admitted >= 1);
    assert!(
        !h.sched
            .store()
            .occurrences_for_schedule(&bad.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
}

/// T40: retention keeps newest 100 + 100, spares live rows and receipts.
#[tokio::test]
async fn t40_retention_caps_with_separate_skip_budget() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 60, t0).await;
    let mut batch = String::new();
    for i in 0..150 {
        batch.push_str(&format!(
            "INSERT INTO schedule_runs (id,schedule_id,slot,trigger,status,created_at,finished_at) \
             VALUES ('t{i}','{}',{i},'scheduled','completed',{i},{i});",
            s.id
        ));
    }
    for i in 0..150 {
        let slot = 1000 + i;
        batch.push_str(&format!(
            "INSERT INTO schedule_runs (id,schedule_id,slot,trigger,status,created_at,finished_at) \
             VALUES ('k{i}','{}',{slot},'scheduled','skipped_overlap',{slot},{slot});",
            s.id
        ));
    }
    for i in 0..5 {
        let slot = 2000 + i;
        batch.push_str(&format!(
            "INSERT INTO schedule_runs (id,schedule_id,slot,trigger,status,created_at) \
             VALUES ('live{i}','{}',{slot},'manual','dispatched',{slot});",
            s.id
        ));
    }
    for i in 0..3 {
        batch.push_str(&format!(
            "INSERT INTO workflow_start_receipts (start_key, run_id, created_at) \
             VALUES ('rk{i}', '00000000-0000-7000-8000-00000000000{i}', {i});"
        ));
    }
    h.sched.store().test_exec(batch).await.unwrap();
    let removed = h.sched.store().prune_schedule(&s.id).await.unwrap();
    assert_eq!(removed, 100);
    let rows = h
        .sched
        .store()
        .occurrences_for_schedule(&s.id, 500)
        .await
        .unwrap();
    assert_eq!(rows.iter().filter(|r| r.status == "completed").count(), 100);
    assert_eq!(
        rows.iter()
            .filter(|r| r.status == "skipped_overlap")
            .count(),
        100
    );
    assert_eq!(rows.iter().filter(|r| r.status == "dispatched").count(), 5);
    for i in 0..3 {
        assert!(
            h.sched
                .engine
                .state_store()
                .lookup_start(&format!("rk{i}"))
                .await
                .unwrap()
                .is_some()
        );
    }
    // Blocked 60 s schedule: new skips still land under the same cap.
    let run = h
        .craft_run("paused-occ", WorkflowStatus::Paused, None)
        .await;
    h.sched
        .store()
        .test_exec(format!(
            "INSERT INTO schedule_runs (id,schedule_id,slot,trigger,status,run_id,created_at) \
             VALUES ('paused-occ','{}',{t0},'scheduled','dispatched','{run}',{t0});",
            s.id,
        ))
        .await
        .unwrap();
    h.tick(h.at(t0, 61)).await;
    h.sched.drain_fires().await;
    let rows = h
        .sched
        .store()
        .occurrences_for_schedule(&s.id, 500)
        .await
        .unwrap();
    assert_eq!(
        rows.iter()
            .filter(|r| r.status == "skipped_overlap")
            .count(),
        100
    );
}

/// T41: migration upgrades a previous-list database without losing rows.
#[tokio::test]
async fn t41_migration_upgrades_without_losing_rows() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.db");
    let run = WorkflowRunId::new();
    let mut state = WorkflowState::new(run, WorkflowId::new("w"), PipelineData);
    state.status = WorkflowStatus::Running;
    state.input = "hello".into();
    let state_json = serde_json::to_string(&state).unwrap();
    let entry = serde_json::to_string(&JournalEntry::Input {
        value: b"in".to_vec(),
    })
    .unwrap();
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE workflow_runs (id TEXT PRIMARY KEY, workflow_id TEXT NOT NULL, \
                status TEXT NOT NULL, current_step TEXT, input TEXT, output TEXT, error TEXT, \
                state_json TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
             CREATE TABLE journal (run_id TEXT NOT NULL, seq INTEGER NOT NULL, \
                entry_type TEXT NOT NULL, payload TEXT NOT NULL, PRIMARY KEY (run_id, seq));
             CREATE TABLE step_state (run_id TEXT NOT NULL, step_id TEXT NOT NULL, \
                status TEXT NOT NULL, attempt INTEGER, last_error TEXT, started_at TEXT, \
                completed_at TEXT, duration_ms INTEGER, input_tokens INTEGER, \
                output_tokens INTEGER, PRIMARY KEY (run_id, step_id));
             CREATE TABLE run_vars (run_id TEXT NOT NULL, key TEXT NOT NULL, value TEXT, \
                PRIMARY KEY (run_id, key));
             PRAGMA user_version = 5;",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO workflow_runs (id, workflow_id, status, input, state_json, created_at, updated_at) \
             VALUES (?1, 'w', 'running', 'hello', ?2, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            rusqlite::params![run.to_string(), state_json],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO journal (run_id, seq, entry_type, payload) VALUES (?1, 1, 'input', ?2)",
            rusqlite::params![run.to_string(), entry],
        )
        .unwrap();
    }
    // Opening migrates (schedule tables land) and old rows survive.
    let sched_store = super::ScheduleStore::open(&path).unwrap();
    let wf_store =
        flare_workflow::sqlite_store::SqliteStore::<flare_workflow::json::PipelineData>::open_file(
            &path,
        )
        .unwrap();
    let loaded = wf_store.load(run).await.unwrap();
    assert_eq!(loaded.input, "hello");
    assert_eq!(wf_store.journal(run).await.unwrap().len(), 1);
    assert!(sched_store.due_schedules(0).await.unwrap().is_empty());
}

/// T42: removal cascades history/queue, keeps receipts, cancels nothing.
#[tokio::test]
async fn t42_removal_cascades_but_keeps_receipts_and_runs() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.tick(h.at(t0, 1)).await;
    h.sched.drain_fires().await;
    // Wait for linkage (dispatched) — driver may still be running.
    let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
    let occ = loop {
        let o = h.occ(&r.occurrence_id).await;
        if o.run_id.is_some() {
            break o;
        }
        assert!(std::time::Instant::now() < end);
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    };
    let run: WorkflowRunId = occ.run_id.clone().unwrap().parse().unwrap();
    assert!(h.sched.store().remove_schedule(&s.id).await.unwrap());
    assert!(
        h.sched
            .store()
            .occurrences_for_schedule(&s.id, 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        h.sched
            .engine
            .state_store()
            .lookup_start(&start_key_for(&r.occurrence_id))
            .await
            .unwrap(),
        Some(run)
    );
    let state = h.sched.engine.state_store().load(run).await.unwrap();
    assert!(!state.status.is_terminal() || state.status != WorkflowStatus::Cancelled);
}

/// T43: updates preserve accepted snapshots; enable resets without catch-up.
#[tokio::test]
async fn t43_update_preserves_snapshots_enable_resets() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "v1", "OUT-1");
    h.echo_wf(&repo, "v2", "OUT-2");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "v1", 300, t0).await;
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.sched
        .store()
        .update_schedule(
            &s.id,
            UpdateInput {
                workflow_name: Some("v2".into()),
                params: Some(serde_json::json!({"x": 1})),
                ..Default::default()
            },
            t0,
        )
        .await
        .unwrap();
    // Accepted occurrence still carries the v1 snapshot.
    let spec = h.occ(&r.occurrence_id).await.parsed_launch_spec().unwrap();
    assert_eq!(spec.workflow_name, "v1");
    // Disable, then enable: next resets to now+every with no catch-up.
    h.sched
        .store()
        .set_enabled(&s.id, false, t0 + 10)
        .await
        .unwrap();
    h.sched
        .store()
        .set_enabled(&s.id, true, t0 + 1000)
        .await
        .unwrap();
    let s2 = h.sched.store().get_by_id(&s.id).await.unwrap().unwrap();
    assert_eq!(s2.next_fire_at, t0 + 1300);
    h.tick(h.at(t0, 1001)).await;
    let rows = h
        .sched
        .store()
        .occurrences_for_schedule(&s.id, 10)
        .await
        .unwrap();
    assert!(rows.iter().all(|o| o.trigger != "scheduled"));
}

/// T44: create/update validation matrix.
#[tokio::test]
async fn t44_validation_matrix() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let base = || CreateInput {
        name: "ok".into(),
        repo_root: h.canon(&repo),
        workflow_name: "w".into(),
        params: None,
        every_secs: 300,
        grace_secs: None,
        overlap: "skip".into(),
    };
    let bad = |mut f: Box<dyn FnMut(&mut CreateInput)>| {
        let mut c = base();
        f(&mut c);
        c
    };
    for c in [
        bad(Box::new(|c| c.name = "".into())),
        bad(Box::new(|c| c.name = "a/b".into())),
        bad(Box::new(|c| c.name = "has space".into())),
        bad(Box::new(|c| c.name = "x".repeat(65))),
        bad(Box::new(|c| c.every_secs = 59)),
        bad(Box::new(|c| c.grace_secs = Some(-1))),
        bad(Box::new(|c| c.overlap = "sometimes".into())),
        bad(Box::new(|c| c.params = Some(serde_json::json!([1])))),
        bad(Box::new(|c| {
            c.params = Some(serde_json::json!({"schedule": 1}))
        })),
        bad(Box::new(|c| c.workflow_name = "missing".into())),
        bad(Box::new(|c| c.repo_root = "/does/not/exist-xyz".into())),
    ] {
        assert!(h.sched.store().create_schedule(c, t0).await.is_err());
    }
    let ok = h.sched.store().create_schedule(base(), t0).await.unwrap();
    assert_eq!(ok.next_fire_at, t0 + 300);
    // Duplicate name in the same repo is rejected.
    assert!(h.sched.store().create_schedule(base(), t0).await.is_err());
    // Bad updates rejected; good update applies.
    assert!(
        h.sched
            .store()
            .update_schedule(
                &ok.id,
                UpdateInput {
                    every_secs: Some(10),
                    ..Default::default()
                },
                t0
            )
            .await
            .is_err()
    );
    let ok2 = h
        .sched
        .store()
        .update_schedule(
            &ok.id,
            UpdateInput {
                every_secs: Some(600),
                ..Default::default()
            },
            t0,
        )
        .await
        .unwrap();
    assert_eq!((ok2.every_secs, ok2.next_fire_at), (600, t0 + 600));
}

/// T49: scheduler instances are independent — never a shared static engine/db.
#[tokio::test]
async fn t49_scheduler_instances_are_independent() {
    let a = Harness::new();
    let b = Harness::new();
    let repo = a.repo("r");
    a.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = a.mk(&repo, "s", "w", 300, t0).await;
    assert!(b.sched.store().get_by_id(&s.id).await.unwrap().is_none());
    assert_ne!(
        a.dir.path().join("workflows.db"),
        b.dir.path().join("workflows.db")
    );
}

/// T49b: the ticker fires schedules on its own, without work-item recovery.
#[tokio::test]
async fn t49b_ticker_runs_independently() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("workflows.db");
    let repo = dir.path().join("r");
    std::fs::create_dir_all(repo.join(".agentflare").join("workflows")).unwrap();
    std::fs::write(
        repo.join(".agentflare").join("workflows").join("w.json"),
        r#"{"name":"w","steps":[{"name":"s","command":["echo","tick"]}]}"#,
    )
    .unwrap();
    let store = super::ScheduleStore::open(&db).unwrap();
    let t0 = Utc::now().timestamp();
    let canon = std::fs::canonicalize(&repo)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let s = store
        .create_schedule(
            CreateInput {
                name: "s".into(),
                repo_root: canon,
                workflow_name: "w".into(),
                params: None,
                every_secs: 300,
                grace_secs: None,
                overlap: "skip".into(),
            },
            t0 - 290,
        )
        .await
        .unwrap();
    // Due ~10 s after open; 1 s test ticks keep this fast.
    let counter = Arc::new(AtomicUsize::new(0));
    let handle = crate::dashboard::server::dispatch_boot::spawn_schedule_ticker_every(
        db.clone(),
        mock_send(counter),
        1,
    );
    let end = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let occ_id = loop {
        let rows = store.occurrences_for_schedule(&s.id, 10).await.unwrap();
        if let Some(o) = rows.iter().find(|o| o.is_terminal()) {
            break o.id.clone();
        }
        assert!(
            std::time::Instant::now() < end,
            "ticker never fired the schedule"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    };
    handle.abort();
    let occ = store.get_occurrence(&occ_id).await.unwrap().unwrap();
    assert_eq!(occ.status, "completed");
}
