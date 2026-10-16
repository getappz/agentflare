//! Scheduler admission/queue/manual tests (item #366 PR-B: T1–T15). Shared
//! harness lives here (`Harness`, mock sends); recovery and lifecycle suites
//! are sibling modules. One temp `workflows.db` per test, injected `now`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use chrono::{DateTime, Utc};
use flare_workflow::WorkflowRunId;
use flare_workflow::json::{PipelineData, SendMessage};
use flare_workflow::types::*;

use super::store::{CreateInput, UpdateInput};
use super::{OccurrenceRow, ScheduleRow, Scheduler, start_key_for};

pub(super) fn mock_send(counter: Arc<AtomicUsize>) -> SendMessage {
    Arc::new(move |inv: flare_workflow::json::StepInvocation| {
        let counter = Arc::clone(&counter);
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok((format!("mock:{}", inv.prompt), 0, 0))
        })
    })
}

pub(super) fn capture_send(
    counter: Arc<AtomicUsize>,
    cwds: Arc<std::sync::Mutex<Vec<Option<PathBuf>>>>,
) -> SendMessage {
    Arc::new(move |inv: flare_workflow::json::StepInvocation| {
        let (counter, cwds) = (Arc::clone(&counter), Arc::clone(&cwds));
        Box::pin(async move {
            counter.fetch_add(1, Ordering::SeqCst);
            cwds.lock().unwrap().push(inv.cwd);
            Ok(("mock".to_string(), 0, 0))
        })
    })
}

pub(super) struct Harness {
    pub(super) dir: tempfile::TempDir,
    pub(super) sched: Scheduler,
}

impl Harness {
    pub(super) fn new() -> Self {
        let counter = Arc::new(AtomicUsize::new(0));
        Self::with_send(mock_send(Arc::clone(&counter)))
    }

    pub(super) fn with_send(send: SendMessage) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("workflows.db");
        let sched = Scheduler::open(&db, send).unwrap();
        Self { dir, sched }
    }

    pub(super) fn repo(&self, name: &str) -> PathBuf {
        let p = self.dir.path().join(name);
        std::fs::create_dir_all(p.join(".agentflare").join("workflows")).unwrap();
        p
    }

    pub(super) fn write_wf(&self, repo: &Path, wf: &str, body: &str) {
        std::fs::write(
            repo.join(".agentflare")
                .join("workflows")
                .join(format!("{wf}.json")),
            body,
        )
        .unwrap();
    }

    pub(super) fn echo_wf(&self, repo: &Path, wf: &str, msg: &str) {
        self.write_wf(
            repo,
            wf,
            &format!(r#"{{"name":"{wf}","steps":[{{"name":"s","command":["echo","{msg}"]}}]}}"#),
        );
    }

    pub(super) fn canon(&self, repo: &Path) -> String {
        std::fs::canonicalize(repo)
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }

    pub(super) async fn mk(
        &self,
        repo: &Path,
        name: &str,
        wf: &str,
        every: i64,
        now: i64,
    ) -> ScheduleRow {
        self.sched
            .store()
            .create_schedule(
                CreateInput {
                    name: name.into(),
                    repo_root: self.canon(repo),
                    workflow_name: wf.into(),
                    params: None,
                    every_secs: every,
                    grace_secs: None,
                    overlap: "skip".into(),
                },
                now,
            )
            .await
            .unwrap()
    }

    pub(super) fn at(&self, base: i64, off: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(base + off, 0).unwrap()
    }

    /// Tick + converge spawned fires (links land; drivers keep running).
    pub(super) async fn tick(&self, now: DateTime<Utc>) -> super::TickSummary {
        let s = self.sched.tick_once(now).await.unwrap();
        self.sched.drain_fires().await;
        s
    }

    pub(super) async fn occ(&self, id: &str) -> OccurrenceRow {
        self.sched
            .store()
            .get_occurrence(id)
            .await
            .unwrap()
            .unwrap()
    }

    pub(super) async fn wait_occ(&self, id: &str) -> OccurrenceRow {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            let o = self.occ(id).await;
            if o.is_terminal() {
                return o;
            }
            assert!(
                std::time::Instant::now() < end,
                "occurrence {id} never settled"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    /// Craft a driverless run + receipt under the occurrence's start key.
    pub(super) async fn craft_run(
        &self,
        occ_id: &str,
        status: WorkflowStatus,
        lease_ago_secs: Option<i64>,
    ) -> WorkflowRunId {
        let run = WorkflowRunId::new();
        let mut s = WorkflowState::new(run, WorkflowId::new("w"), PipelineData);
        s.status = status;
        s.lease = lease_ago_secs.map(|ago| ExecutorLease {
            owner: "dead:1:x".into(),
            renewed_at: Utc::now() - chrono::Duration::seconds(ago),
        });
        self.sched
            .engine
            .state_store()
            .create_start_once(
                start_key_for(occ_id),
                s,
                JournalEntry::Input { value: vec![] },
            )
            .await
            .unwrap();
        run
    }

    pub(super) async fn backdate_dispatch(&self, occ_id: &str, ts: i64) {
        self.sched
            .store()
            .test_exec(format!(
                "UPDATE schedule_runs SET dispatch_started_at = {ts} WHERE id = '{occ_id}';"
            ))
            .await
            .unwrap();
    }
}

/// T1: due/disabled/frozen filtering.
#[tokio::test]
async fn t01_due_disabled_frozen_filtering() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let due = h.mk(&repo, "due", "w", 300, t0).await;
    let mut dis = h.mk(&repo, "dis", "w", 300, t0).await;
    dis = h
        .sched
        .store()
        .set_enabled(&dis.id, false, t0)
        .await
        .unwrap();
    assert!(!dis.enabled);
    let frozen = h.mk(&repo, "froz", "w", 300, t0).await;
    h.sched
        .store()
        .test_exec(format!(
            "UPDATE schedules SET frozen_error = 'bad' WHERE id = '{}';",
            frozen.id
        ))
        .await
        .unwrap();
    let future = h.mk(&repo, "future", "w", 300, t0 + 10_000).await;
    let _ = future;
    let due_list = h.sched.store().due_schedules(t0 + 310).await.unwrap();
    assert_eq!(
        due_list.iter().map(|s| s.name.clone()).collect::<Vec<_>>(),
        vec!["due"]
    );
    let _ = due;
}

/// T2: within grace admits exactly one occurrence; `last_fire_at == slot`.
#[tokio::test]
async fn t02_within_grace_admits_once_with_slot_last_fire() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    let slot = t0 + 300;
    let out = h
        .sched
        .store()
        .claim_due_slot(&s.id, slot + 10)
        .await
        .unwrap();
    let id = match out {
        super::store::SlotOutcome::Admitted { occurrence_id } => occurrence_id,
        o => panic!("expected admission, got {o:?}"),
    };
    let s2 = h.sched.store().get_by_id(&s.id).await.unwrap().unwrap();
    assert_eq!(s2.last_fire_at, Some(slot));
    assert_eq!(s2.next_fire_at, slot + 300);
    let occ = h.occ(&id).await;
    assert_eq!(
        (occ.slot, occ.trigger.as_str(), occ.status.as_str()),
        (slot, "scheduled", "queued")
    );
    // Exactly one: clock advanced past the slot.
    assert!(matches!(
        h.sched
            .store()
            .claim_due_slot(&s.id, slot + 10)
            .await
            .unwrap(),
        super::store::SlotOutcome::NotDue
    ));
    assert_eq!(
        h.sched
            .store()
            .occurrences_for_schedule(&s.id, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// T3: 40 s late on a 300 s schedule keeps the original grid.
#[tokio::test]
async fn t03_late_tick_keeps_original_grid() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    h.sched
        .store()
        .claim_due_slot(&s.id, t0 + 340)
        .await
        .unwrap();
    let s2 = h.sched.store().get_by_id(&s.id).await.unwrap().unwrap();
    assert_eq!(s2.next_fire_at, t0 + 600);
    assert_eq!(s2.last_fire_at, Some(t0 + 300));
}

/// T4: two-slot gap within explicit grace admits once, no backfill.
#[tokio::test]
async fn t04_gap_within_grace_admits_once_no_backfill() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let mut s = h.mk(&repo, "s", "w", 300, t0).await;
    s = h
        .sched
        .store()
        .update_schedule(
            &s.id,
            UpdateInput {
                grace_secs: Some(Some(700)),
                ..Default::default()
            },
            t0,
        )
        .await
        .unwrap();
    assert_eq!(s.effective_grace_secs(), 700);
    h.sched
        .store()
        .claim_due_slot(&s.id, t0 + 900)
        .await
        .unwrap();
    let rows = h
        .sched
        .store()
        .occurrences_for_schedule(&s.id, 10)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].slot, t0 + 300);
    let s2 = h.sched.store().get_by_id(&s.id).await.unwrap().unwrap();
    assert_eq!(s2.next_fire_at, t0 + 1200);
}

/// T5: grace-boundary admission + overflow rejection.
#[tokio::test]
async fn t05_grace_boundaries_and_overflow_rejection() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    // Exactly at grace: admitted; one past: skipped.
    let a = h.mk(&repo, "a", "w", 300, t0).await;
    let out = h
        .sched
        .store()
        .claim_due_slot(&a.id, t0 + 600)
        .await
        .unwrap();
    assert!(matches!(out, super::store::SlotOutcome::Admitted { .. }));
    let b = h.mk(&repo, "b", "w", 300, t0).await;
    let out = h
        .sched
        .store()
        .claim_due_slot(&b.id, t0 + 601)
        .await
        .unwrap();
    assert!(matches!(
        out,
        super::store::SlotOutcome::SkippedMissed { .. }
    ));
    // Overflow rejected at create and update.
    let bad = h
        .sched
        .store()
        .create_schedule(
            CreateInput {
                name: "huge".into(),
                repo_root: h.canon(&repo),
                workflow_name: "w".into(),
                params: None,
                every_secs: i64::MAX,
                grace_secs: None,
                overlap: "skip".into(),
            },
            t0,
        )
        .await;
    assert!(bad.is_err());
    let err = h
        .sched
        .store()
        .update_schedule(
            &a.id,
            UpdateInput {
                every_secs: Some(i64::MAX),
                ..Default::default()
            },
            t0,
        )
        .await
        .unwrap_err();
    assert!(err.contains("overflow"));
}

/// T6: default grace + 2 h outage → `skipped_missed`.
#[tokio::test]
async fn t06_default_grace_outage_skips() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    assert_eq!(s.effective_grace_secs(), 300);
    let out = h
        .sched
        .store()
        .claim_due_slot(&s.id, t0 + 300 + 7200)
        .await
        .unwrap();
    let id = match out {
        super::store::SlotOutcome::SkippedMissed { occurrence_id } => occurrence_id,
        o => panic!("got {o:?}"),
    };
    let occ = h.occ(&id).await;
    assert!(occ.finished_at.is_some() && occ.launch_spec.is_none());
}

/// T7: explicit 4 h grace fires once after the same outage.
#[tokio::test]
async fn t07_explicit_grace_fires_once_after_outage() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    h.sched
        .store()
        .update_schedule(
            &s.id,
            UpdateInput {
                grace_secs: Some(Some(14400)),
                ..Default::default()
            },
            t0,
        )
        .await
        .unwrap();
    let out = h
        .sched
        .store()
        .claim_due_slot(&s.id, t0 + 300 + 7200)
        .await
        .unwrap();
    assert!(matches!(out, super::store::SlotOutcome::Admitted { .. }));
    assert_eq!(
        h.sched
            .store()
            .occurrences_for_schedule(&s.id, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

/// T8: grace=0 admits at the exact slot, skips any later.
#[tokio::test]
async fn t08_zero_grace_exact_slot_only() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let a = h.mk(&repo, "a", "w", 300, t0).await;
    h.sched
        .store()
        .update_schedule(
            &a.id,
            UpdateInput {
                grace_secs: Some(Some(0)),
                ..Default::default()
            },
            t0,
        )
        .await
        .unwrap();
    let out = h
        .sched
        .store()
        .claim_due_slot(&a.id, t0 + 300)
        .await
        .unwrap();
    assert!(matches!(out, super::store::SlotOutcome::Admitted { .. }));
    let b = h.mk(&repo, "b", "w", 300, t0).await;
    h.sched
        .store()
        .update_schedule(
            &b.id,
            UpdateInput {
                grace_secs: Some(Some(0)),
                ..Default::default()
            },
            t0,
        )
        .await
        .unwrap();
    let out = h
        .sched
        .store()
        .claim_due_slot(&b.id, t0 + 301)
        .await
        .unwrap();
    assert!(matches!(
        out,
        super::store::SlotOutcome::SkippedMissed { .. }
    ));
}

/// T9: one open occurrence blocks three subsequent slots (skips accumulate).
#[tokio::test]
async fn t09_open_occurrence_blocks_three_slots() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 60, t0).await;
    let slot = t0 + 60;
    assert!(matches!(
        h.sched.store().claim_due_slot(&s.id, slot).await.unwrap(),
        super::store::SlotOutcome::Admitted { .. }
    ));
    for i in 1..=3 {
        assert!(
            matches!(
                h.sched
                    .store()
                    .claim_due_slot(&s.id, slot + 60 * i)
                    .await
                    .unwrap(),
                super::store::SlotOutcome::SkippedOverlap { .. }
            ),
            "slot {i} must skip on overlap"
        );
    }
    let rows = h
        .sched
        .store()
        .occurrences_for_schedule(&s.id, 10)
        .await
        .unwrap();
    assert_eq!(rows.iter().filter(|r| r.status == "queued").count(), 1);
    assert_eq!(
        rows.iter()
            .filter(|r| r.status == "skipped_overlap")
            .count(),
        3
    );
}

/// T10: queued, unlinked dispatching, and dispatched runs each block overlap.
#[tokio::test]
async fn t10_each_open_state_blocks_overlap() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    // Queued blocks.
    let q = h.mk(&repo, "q", "w", 60, t0).await;
    h.sched
        .store()
        .claim_due_slot(&q.id, t0 + 60)
        .await
        .unwrap();
    assert!(matches!(
        h.sched
            .store()
            .claim_due_slot(&q.id, t0 + 120)
            .await
            .unwrap(),
        super::store::SlotOutcome::SkippedOverlap { .. }
    ));
    // Unlinked dispatching blocks.
    let d = h.mk(&repo, "d", "w", 60, t0).await;
    h.sched
        .store()
        .claim_due_slot(&d.id, t0 + 60)
        .await
        .unwrap();
    h.sched.store().claim_queued(t0 + 60).await.unwrap();
    assert!(matches!(
        h.sched
            .store()
            .claim_due_slot(&d.id, t0 + 120)
            .await
            .unwrap(),
        super::store::SlotOutcome::SkippedOverlap { .. }
    ));
    // Dispatched (linked, e.g. paused) blocks.
    let p = h.mk(&repo, "p", "w", 60, t0).await;
    let out = h
        .sched
        .store()
        .claim_due_slot(&p.id, t0 + 60)
        .await
        .unwrap();
    let occ_id = match out {
        super::store::SlotOutcome::Admitted { occurrence_id } => occurrence_id,
        o => panic!("got {o:?}"),
    };
    h.sched
        .store()
        .link_run(&occ_id, "bogus-run")
        .await
        .unwrap();
    assert!(matches!(
        h.sched
            .store()
            .claim_due_slot(&p.id, t0 + 120)
            .await
            .unwrap(),
        super::store::SlotOutcome::SkippedOverlap { .. }
    ));
}

/// T11: `overlap=allow` admits additional occurrences.
#[tokio::test]
async fn t11_overlap_allow_admits() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let mut s = h.mk(&repo, "s", "w", 60, t0).await;
    s = h
        .sched
        .store()
        .update_schedule(
            &s.id,
            UpdateInput {
                overlap: Some("allow".into()),
                ..Default::default()
            },
            t0,
        )
        .await
        .unwrap();
    assert_eq!(s.overlap, "allow");
    for i in 0..3 {
        assert!(matches!(
            h.sched
                .store()
                .claim_due_slot(&s.id, t0 + 60 * (i + 1))
                .await
                .unwrap(),
            super::store::SlotOutcome::Admitted { .. }
        ));
    }
}

/// T12: two concurrent queue claimers select one queued occurrence once.
#[tokio::test]
async fn t12_concurrent_queue_claim_single_winner() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let (store, barrier) = (h.sched.store().clone(), Arc::clone(&barrier));
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            store.claim_queued(t0).await.unwrap().len()
        }));
    }
    let mut total = 0;
    for t in tasks {
        total += t.await.unwrap();
    }
    assert_eq!(total, 1);
}

/// T13: repeated manual fires in the same second get distinct IDs.
#[tokio::test]
async fn t13_manual_fires_same_second_distinct_ids() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    let mut ids = Vec::new();
    for _ in 0..3 {
        let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
        assert_eq!(r.status, "queued");
        ids.push(r.occurrence_id);
    }
    assert_eq!(ids.len(), 3);
    let uniq: std::collections::HashSet<_> = ids.iter().collect();
    assert_eq!(uniq.len(), 3);
}

/// T14: manual fire bypasses disabled/grace/overlap and preserves the clock.
#[tokio::test]
async fn t14_manual_bypasses_checks_preserves_clock() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    // Open occurrence (overlap) + disable + far past grace.
    h.sched
        .store()
        .claim_due_slot(&s.id, t0 + 300)
        .await
        .unwrap();
    h.sched
        .store()
        .set_enabled(&s.id, false, t0 + 300)
        .await
        .unwrap();
    let before = h.sched.store().get_by_id(&s.id).await.unwrap().unwrap();
    let r = h
        .sched
        .store()
        .enqueue_manual(&s.id, t0 + 10_000)
        .await
        .unwrap();
    let occ = h.occ(&r.occurrence_id).await;
    assert_eq!(
        (occ.trigger.as_str(), occ.status.as_str()),
        ("manual", "queued")
    );
    assert_eq!(occ.slot, t0 + 10_000);
    assert!(occ.start_deadline_at.is_none());
    let after = h.sched.store().get_by_id(&s.id).await.unwrap().unwrap();
    assert_eq!(
        (after.next_fire_at, after.last_fire_at),
        (before.next_fire_at, before.last_fire_at)
    );
}

/// T15: daemon-offline manual request survives delay+grace, executes at tick.
#[tokio::test]
async fn t15_offline_manual_survives_and_executes() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hello-t15");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    // Far past the 120 s recovery delay and the schedule grace.
    let summary = h.tick(h.at(t0, 5000)).await;
    assert!(summary.fired >= 1);
    let occ = h.wait_occ(&r.occurrence_id).await;
    assert_eq!(occ.status, "completed");
    assert!(occ.run_id.is_some());
}
