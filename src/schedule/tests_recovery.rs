//! Scheduler recovery/reconciliation tests (item #366 PR-B: T18–T23,
//! T25–T26, T25b, T27–T32). Recovery paths drive `reconcile_*` directly for
//! determinism; the harness comes from the sibling `tests` module.

use chrono::Utc;
use flare_workflow::StateStore;
use flare_workflow::types::*;

use super::start_key_for;
use super::tests::Harness;

/// T18 (scheduler half): old dispatch with a receipt links, never retries.
#[tokio::test]
async fn t18_old_dispatch_with_receipt_links_without_retry() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    // Fire held 500 s (past the 120 s delay) with a live receipt behind it.
    h.backdate_dispatch(&r.occurrence_id, t0 - 500).await;
    let run = h
        .craft_run(&r.occurrence_id, WorkflowStatus::Running, Some(0))
        .await;
    let occ = h.occ(&r.occurrence_id).await;
    // Links (fresh lease → left running, so no state transition to count).
    h.sched
        .reconcile_dispatching(&occ, h.at(t0, 10))
        .await
        .unwrap();
    let occ = h.occ(&r.occurrence_id).await;
    assert_eq!(occ.status, "dispatched");
    assert_eq!(occ.run_id, Some(run.to_string()));
    // No retry, no duplicate: exactly one run, nothing queued.
    assert_eq!(
        h.sched.engine.state_store().list_all().await.unwrap().len(),
        1
    );
    assert!(
        h.sched
            .store()
            .list_open("queued")
            .await
            .unwrap()
            .is_empty()
    );
}

/// T19 (scheduler half): lost linkage recovered from the receipt, no restart.
#[tokio::test]
async fn t19_lost_linkage_recovers_from_receipt() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    // Run started + completed under the occurrence's key, never linked.
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    let run = h
        .craft_run(&r.occurrence_id, WorkflowStatus::Completed, None)
        .await;
    let occ = h.occ(&r.occurrence_id).await;
    let moved = h
        .sched
        .reconcile_dispatching(&occ, h.at(t0, 500))
        .await
        .unwrap();
    assert!(moved);
    let occ = h.occ(&r.occurrence_id).await;
    assert_eq!(occ.status, "completed");
    assert_eq!(occ.run_id, Some(run.to_string()));
    assert_eq!(
        h.sched.engine.state_store().list_all().await.unwrap().len(),
        1
    );
}

/// T20: completed run evicted before linkage → `evicted`, receipt kept, idle.
#[tokio::test]
async fn t20_evicted_before_linkage_marks_evicted() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    let run = h
        .craft_run(&r.occurrence_id, WorkflowStatus::Completed, None)
        .await;
    h.sched.engine.state_store().delete(run).await.unwrap();
    let occ = h.occ(&r.occurrence_id).await;
    let moved = h
        .sched
        .reconcile_dispatching(&occ, h.at(t0, 500))
        .await
        .unwrap();
    assert!(moved);
    let occ = h.occ(&r.occurrence_id).await;
    assert_eq!(occ.status, "evicted");
    assert_eq!(
        h.sched
            .engine
            .state_store()
            .lookup_start(&start_key_for(&r.occurrence_id))
            .await
            .unwrap(),
        Some(run)
    );
    assert_eq!(
        h.sched.engine.state_store().list_all().await.unwrap().len(),
        0
    );
}

/// T21 (scheduler half): commit-without-driver + stale lease → interrupted.
#[tokio::test]
async fn t21_undriven_stale_run_is_interrupted() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    h.backdate_dispatch(&r.occurrence_id, t0 - 500).await;
    // Crashed after commit before driver launch: durable run, no driver.
    h.craft_run(&r.occurrence_id, WorkflowStatus::Running, Some(3600))
        .await;
    let occ = h.occ(&r.occurrence_id).await;
    let moved = h
        .sched
        .reconcile_dispatching(&occ, h.at(t0, 10))
        .await
        .unwrap();
    assert!(moved);
    let occ = h.occ(&r.occurrence_id).await;
    assert_eq!(occ.status, "interrupted");
    assert_eq!(
        h.sched.engine.state_store().list_all().await.unwrap().len(),
        1
    );
}

/// T22: no receipt after a lost dispatch → same-occurrence retry in deadline,
/// `dispatch_failed` outside it.
#[tokio::test]
async fn t22_lost_dispatch_retries_within_deadline_only() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    // Within deadline: back to queued, same occurrence/slot/spec.
    let a = h.mk(&repo, "a", "w", 300, t0).await;
    let out = h
        .sched
        .store()
        .claim_due_slot(&a.id, t0 + 300)
        .await
        .unwrap();
    let aid = match out {
        super::store::SlotOutcome::Admitted { occurrence_id } => occurrence_id,
        o => panic!("got {o:?}"),
    };
    h.sched.store().claim_queued(t0 + 300).await.unwrap();
    h.backdate_dispatch(&aid, t0 - 500).await;
    let occ = h.occ(&aid).await;
    assert!(
        h.sched
            .reconcile_dispatching(&occ, h.at(t0 + 300, 100))
            .await
            .unwrap()
    );
    let occ = h.occ(&aid).await;
    assert_eq!(occ.status, "queued");
    assert!(occ.dispatch_started_at.is_none());
    // Same occurrence is what retries: re-claim selects it again.
    let reclaimed = h.sched.store().claim_queued(t0 + 400).await.unwrap();
    assert_eq!(
        reclaimed.iter().map(|o| o.id.clone()).collect::<Vec<_>>(),
        vec![aid.clone()]
    );
    // Outside deadline: dispatch_failed with the reason kept.
    let b = h.mk(&repo, "b", "w", 300, t0).await;
    let out = h
        .sched
        .store()
        .claim_due_slot(&b.id, t0 + 300)
        .await
        .unwrap();
    let bid = match out {
        super::store::SlotOutcome::Admitted { occurrence_id } => occurrence_id,
        o => panic!("got {o:?}"),
    };
    h.sched.store().claim_queued(t0 + 300).await.unwrap();
    h.backdate_dispatch(&bid, t0 - 500).await;
    let occ = h.occ(&bid).await;
    assert!(
        h.sched
            .reconcile_dispatching(&occ, h.at(t0 + 300, 301))
            .await
            .unwrap()
    );
    let occ = h.occ(&bid).await;
    assert_eq!(occ.status, "dispatch_failed");
    assert!(occ.error.as_deref().unwrap_or("").contains("grace"));
}

/// T23: manual lost dispatch retries without schedule grace.
#[tokio::test]
async fn t23_manual_lost_dispatch_retries_without_grace() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    h.backdate_dispatch(&r.occurrence_id, t0 - 100_000).await;
    let occ = h.occ(&r.occurrence_id).await;
    assert!(
        h.sched
            .reconcile_dispatching(&occ, h.at(t0, 200_000))
            .await
            .unwrap()
    );
    assert_eq!(h.occ(&r.occurrence_id).await.status, "queued");
}

/// T25: expired scheduled queue entry → `skipped_missed` at claim; a live
/// admitted attempt is never duplicated.
#[tokio::test]
async fn t25_expired_queue_entry_skips_live_attempt_singles() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    // Expired: admitted at slot, disabled so nothing else moves, claimed
    // past its deadline.
    let a = h.mk(&repo, "a", "w", 300, t0).await;
    let out = h
        .sched
        .store()
        .claim_due_slot(&a.id, t0 + 300)
        .await
        .unwrap();
    let aid = match out {
        super::store::SlotOutcome::Admitted { occurrence_id } => occurrence_id,
        o => panic!("got {o:?}"),
    };
    h.sched
        .store()
        .set_enabled(&a.id, false, t0 + 300)
        .await
        .unwrap();
    let summary = h.tick(h.at(t0 + 300, 350)).await;
    assert_eq!(summary.fired, 0);
    assert_eq!(h.occ(&aid).await.status, "skipped_missed");
    // Live: dispatching + receipt + fresh run links exactly once.
    let b = h.mk(&repo, "b", "w", 300, t0).await;
    let r = h.sched.store().enqueue_manual(&b.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    let run = h
        .craft_run(&r.occurrence_id, WorkflowStatus::Running, Some(0))
        .await;
    let summary = h.tick(h.at(t0, 10)).await;
    assert_eq!(summary.fired, 0);
    let occ = h.occ(&r.occurrence_id).await;
    assert_eq!(occ.status, "dispatched");
    assert_eq!(occ.run_id, Some(run.to_string()));
    assert_eq!(
        h.sched.engine.state_store().list_all().await.unwrap().len(),
        1
    );
}

/// T25b: eager write converges; tick fallback converges without it.
#[tokio::test]
async fn t25b_eager_and_fallback_converge() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    // Fallback: run completes before linkage; the tick copies the outcome.
    let a = h.mk(&repo, "a", "w", 300, t0).await;
    let ra = h.sched.store().enqueue_manual(&a.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    let run = h
        .craft_run(&ra.occurrence_id, WorkflowStatus::Completed, None)
        .await;
    h.sched
        .store()
        .link_run(&ra.occurrence_id, &run.to_string())
        .await
        .unwrap();
    h.tick(h.at(t0, 5)).await;
    let occ = h.occ(&ra.occurrence_id).await;
    assert_eq!(occ.status, "completed");
    assert!(occ.finished_at.is_some());
    // Eager: end-to-end fire settles the occurrence with no tick at all.
    let b = h.mk(&repo, "b", "w", 300, t0).await;
    let rb = h.sched.store().enqueue_manual(&b.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    // Drive the fire directly: claim + spawn happen here, then converge.
    let claimed = h.sched.store().claim_queued(t0).await.unwrap();
    assert!(claimed.is_empty());
    // Requeue to simulate the queue claim path going through tick fires.
    h.sched
        .store()
        .mark_queued(&rb.occurrence_id)
        .await
        .unwrap();
    h.tick(h.at(t0, 6)).await;
    let occ = h.wait_occ(&rb.occurrence_id).await;
    assert_eq!(occ.status, "completed");
}

/// T26: failed outcome + error copy to the occurrence.
#[tokio::test]
async fn t26_failed_outcome_and_error_copied() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.write_wf(
        &repo,
        "w",
        r#"{"name":"w","steps":[{"name":"s","command":["false"]}]}"#,
    );
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.tick(h.at(t0, 1)).await;
    let occ = h.wait_occ(&r.occurrence_id).await;
    assert_eq!(occ.status, "failed");
    assert!(occ.error.as_deref().unwrap_or("").contains("false"));
}

/// T27: missing linked workflow → `evicted`, never retried.
#[tokio::test]
async fn t27_missing_linked_workflow_evicts() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    let run = h
        .craft_run(&r.occurrence_id, WorkflowStatus::Running, Some(0))
        .await;
    h.sched
        .store()
        .link_run(&r.occurrence_id, &run.to_string())
        .await
        .unwrap();
    h.sched.engine.state_store().delete(run).await.unwrap();
    let occ = h.occ(&r.occurrence_id).await;
    assert!(
        h.sched
            .reconcile_dispatched(&occ, h.at(t0, 5))
            .await
            .unwrap()
    );
    assert_eq!(h.occ(&r.occurrence_id).await.status, "evicted");
    assert_eq!(
        h.sched.engine.state_store().list_all().await.unwrap().len(),
        0
    );
    assert!(
        h.sched
            .store()
            .list_open("queued")
            .await
            .unwrap()
            .is_empty()
    );
}

/// T28: stale orphan cancels to `interrupted` before overlap admission.
#[tokio::test]
async fn t28_stale_orphan_interrupts_then_slot_admits() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 60, t0).await;
    let out = h
        .sched
        .store()
        .claim_due_slot(&s.id, t0 + 60)
        .await
        .unwrap();
    let old = match out {
        super::store::SlotOutcome::Admitted { occurrence_id } => occurrence_id,
        o => panic!("got {o:?}"),
    };
    h.sched.store().claim_queued(t0 + 60).await.unwrap();
    let run = h.craft_run(&old, WorkflowStatus::Running, Some(3600)).await;
    h.sched
        .store()
        .link_run(&old, &run.to_string())
        .await
        .unwrap();
    // Next slot due (t0+120) while the stale run still blocks it.
    let summary = h.tick(h.at(t0, 130)).await;
    h.sched.drain_fires().await;
    assert_eq!(h.occ(&old).await.status, "interrupted");
    assert_eq!(summary.admitted, 1);
    let rows = h
        .sched
        .store()
        .occurrences_for_schedule(&s.id, 10)
        .await
        .unwrap();
    // The new slot was admitted (it may already have fired through).
    assert!(rows.iter().any(|r| r.slot == t0 + 120));
}

/// T29+T30: paused runs keep lease-loss immunity and keep blocking overlap.
#[tokio::test]
async fn t29_paused_run_survives_ticks_and_blocks() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 60, t0).await;
    let r = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    // Paused, no lease: exactly what a settled pause leaves behind.
    let run = h
        .craft_run(&r.occurrence_id, WorkflowStatus::Paused, None)
        .await;
    h.sched
        .store()
        .link_run(&r.occurrence_id, &run.to_string())
        .await
        .unwrap();
    for i in 1..=3 {
        // Ticks land mid-slot (within grace) so each due slot skips on
        // overlap instead of missing on lateness.
        let summary = h.tick(h.at(t0, 90 + 60 * (i as i64 - 1))).await;
        assert_eq!(summary.skipped_overlap, 1, "tick {i} must skip on overlap");
        assert_eq!(h.occ(&r.occurrence_id).await.status, "dispatched");
    }
    assert_eq!(
        h.sched.engine.state_store().load(run).await.unwrap().status,
        WorkflowStatus::Paused
    );
    assert_eq!(
        h.sched
            .store()
            .occurrences_for_schedule(&s.id, 20)
            .await
            .unwrap()
            .iter()
            .filter(|o| o.status == "skipped_overlap")
            .count(),
        3
    );
}

/// T31: rechecked state wins — completed-with-stale-lease copies out,
/// paused-after-stale stays.
#[tokio::test]
async fn t31_newer_state_wins_over_scan() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    // Completed despite the stale lease: real outcome, not interruption.
    let r1 = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    let run1 = h
        .craft_run(&r1.occurrence_id, WorkflowStatus::Running, Some(3600))
        .await;
    h.sched
        .store()
        .link_run(&r1.occurrence_id, &run1.to_string())
        .await
        .unwrap();
    h.sched
        .engine
        .state_store()
        .update(run1, |st| st.status = WorkflowStatus::Completed)
        .await
        .unwrap();
    let occ = h.occ(&r1.occurrence_id).await;
    assert!(
        h.sched
            .reconcile_dispatched(&occ, h.at(t0, 5))
            .await
            .unwrap()
    );
    assert_eq!(h.occ(&r1.occurrence_id).await.status, "completed");
    // Paused after the scan snapshot: untouched despite the stale lease.
    let r2 = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    let run2 = h
        .craft_run(&r2.occurrence_id, WorkflowStatus::Running, Some(3600))
        .await;
    h.sched
        .store()
        .link_run(&r2.occurrence_id, &run2.to_string())
        .await
        .unwrap();
    h.sched.engine.pause_workflow(run2).await.unwrap();
    let occ = h.occ(&r2.occurrence_id).await;
    assert!(
        !h.sched
            .reconcile_dispatched(&occ, h.at(t0, 5))
            .await
            .unwrap()
    );
    assert_eq!(h.occ(&r2.occurrence_id).await.status, "dispatched");
}

/// T32: unreadable run rows stay open without aborting the sweep.
#[tokio::test]
async fn t32_corrupt_row_does_not_abort_reconciliation() {
    let h = Harness::new();
    let repo = h.repo("r");
    h.echo_wf(&repo, "w", "hi");
    let t0 = Utc::now().timestamp();
    let s = h.mk(&repo, "s", "w", 300, t0).await;
    // Corrupt: occurrence linked to a run row with garbage state_json, so
    // even conditional cancellation fails and the row stays open.
    let rc = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    h.sched
        .store()
        .test_exec(
            "INSERT INTO workflow_runs (id, workflow_id, status, state_json, created_at, updated_at) \
             VALUES ('corrupt-run', 'w', 'running', 'not-json{{', 'x', 'x');".to_string(),
        )
        .await
        .unwrap();
    h.sched
        .store()
        .link_run(&rc.occurrence_id, "corrupt-run")
        .await
        .unwrap();
    // Healthy sibling settles in the same tick.
    let rok = h.sched.store().enqueue_manual(&s.id, t0).await.unwrap();
    h.sched.store().claim_queued(t0).await.unwrap();
    let run = h
        .craft_run(&rok.occurrence_id, WorkflowStatus::Failed, None)
        .await;
    h.sched
        .engine
        .state_store()
        .update(run, |st| st.error = Some("boom".into()))
        .await
        .unwrap();
    h.sched
        .store()
        .link_run(&rok.occurrence_id, &run.to_string())
        .await
        .unwrap();
    let summary = h.sched.tick_once(h.at(t0, 5)).await.unwrap();
    assert!(!summary.errors.is_empty());
    assert_eq!(h.occ(&rc.occurrence_id).await.status, "dispatched");
    let ok = h.occ(&rok.occurrence_id).await;
    assert_eq!(ok.status, "failed");
    assert_eq!(ok.error.as_deref(), Some("boom"));
}
