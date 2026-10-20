//! CodeRabbit review-repair announce-once + completion-summary tests (item #633
//! follow-up). Split out of `supervisor_tests.rs`, which is frozen — do not
//! grow that file; add repair-cycle tests here instead.

use super::tests::{
    coderabbit_finding, seed_gate_label, seed_in_review_item_with_claim_age, test_auth_conn,
    test_mcp, test_queue,
};
use super::*;

#[test]
fn promoting_a_merged_item_cancels_its_queued_dispatch() {
    let mut mcp = test_mcp();
    let queue = test_queue();
    mcp.job_queue_override = Some(queue.clone());
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let job = queue
        .enqueue(
            &agentflare_jobs::AgentJob::new("agentflare-work")
                .args([item_id.clone(), "claude-code".to_string()])
                .in_process(),
        )
        .unwrap();
    assert!(
        mcp.promote_verified_merged_item(&item_id, &item, std::path::Path::new("/repo"))
            .unwrap()
    );
    assert_eq!(
        queue.get(&job.id).unwrap().state,
        agentflare_jobs::JobState::Killed
    );
}

#[test]
fn coderabbit_never_repairs_a_merged_pr() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    mcp.with_backend_db(|conn| {
        crate::mcp_server::merge_item_metadata(conn, &item_id, |m| {
            m.insert(
                "pr".into(),
                serde_json::json!({"number": 1, "status": "merged"}),
            );
        })
        .unwrap();
    })
    .unwrap();
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    assert!(matches!(
        coderabbit_repair_or_gate(
            &mcp,
            &queue,
            &test_auth_conn(),
            agentflare_resource_gate::Policy::Normal,
            &item,
            1,
            &[coderabbit_finding(1, "coderabbitai[bot]")],
            &[],
            &seed_gate_label(&mcp),
            "/repo",
        ),
        SelfRepairOutcome::Skipped
    ));
    assert!(
        queue
            .list(Some(agentflare_jobs::JobState::Queued))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn coderabbit_findings_fingerprint_ignores_order_but_not_content() {
    let a = coderabbit_finding(1, "coderabbitai[bot]");
    let mut b = coderabbit_finding(2, "coderabbitai[bot]");
    b.path = "src/other.rs".to_string();
    let fp_ab = coderabbit_findings_fingerprint(&[a.clone(), b.clone()]);
    let fp_ba = coderabbit_findings_fingerprint(&[b.clone(), a.clone()]);
    assert_eq!(fp_ab, fp_ba, "reordered fetches must not re-announce");
    let mut changed = b.clone();
    changed.body = "a different ask entirely".to_string();
    assert_ne!(
        fp_ab,
        coderabbit_findings_fingerprint(&[a, changed]),
        "changed findings must re-announce"
    );
    assert!(
        coderabbit_findings_fingerprint(&[]).is_empty(),
        "no findings must fingerprint empty (never announced, never completed)"
    );
}

#[test]
fn coderabbit_repair_or_gate_redispatches_a_round_whose_job_was_lost() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let label_id_by_name = seed_gate_label(&mcp);
    let auth_conn = test_auth_conn();
    let findings = vec![
        coderabbit_finding(1, "coderabbitai[bot]"),
        coderabbit_finding(2, "coderabbitai[bot]"),
    ];

    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let outcome = coderabbit_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        &findings,
        &[],
        &label_id_by_name,
        "/repo",
    );
    assert!(matches!(outcome, SelfRepairOutcome::Dispatched));
    // Repair job finished without posting success (failed attempt) — cancel it
    // so the retry isn't mistaken for a job still in flight. (`dequeue` would
    // move it to `running`, which `job_in_flight` still counts; the closure
    // is a keep-filter, so `false` cancels everything for the item.)
    queue.cancel_for_item(&item_id, |_| false).unwrap();

    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let outcome = coderabbit_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        &findings,
        &[],
        &label_id_by_name,
        "/repo",
    );

    assert!(
        matches!(outcome, SelfRepairOutcome::Dispatched),
        "a recorded round whose job exited without an answer must be retried, not skipped forever"
    );
    assert_eq!(
        queue
            .list(Some(agentflare_jobs::JobState::Queued))
            .unwrap()
            .len(),
        1,
        "the retry must be queued"
    );
    // Retries are bounded: the claimed-then-lost attempts count toward the cap.
}

fn tick(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    item_id: &str,
    review: &ReviewBotState,
    labels: &[String],
) -> PassingPrOutcome {
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, item_id).unwrap())
        .unwrap();
    let auto = crate::worktree::AutoMergeRef::default();
    merge_or_repair_findings(
        mcp,
        queue,
        &test_auth_conn(),
        agentflare_resource_gate::Policy::Normal,
        &item,
        std::path::Path::new("/repo"),
        42,
        review,
        labels,
        &seed_gate_label(mcp),
        "/repo",
        CiGreenMerge::Allowed {
            head_sha: None,
            auto_merge: &auto,
        },
    )
}

fn queued(queue: &agentflare_jobs::Queue) -> usize {
    queue
        .list(Some(agentflare_jobs::JobState::Queued))
        .unwrap()
        .len()
}

#[test]
fn approved_green_pr_with_lost_repair_job_is_redispatched_within_the_cap() {
    // "claimed, job exited, threads open, label on", through the real entry.
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let labels = vec![PR_APPROVAL_LABEL.to_string()];
    let review = ReviewBotState::for_dispatch(vec![coderabbit_finding(1, "coderabbitai[bot]")]);

    let first = tick(&mcp, &queue, &item_id, &review, &labels);
    assert!(matches!(
        first,
        PassingPrOutcome::Repair(SelfRepairOutcome::Dispatched)
    ));
    // A live (queued) repair job: no behaviour change, nothing new queued.
    let live = tick(&mcp, &queue, &item_id, &review, &labels);
    assert!(matches!(
        live,
        PassingPrOutcome::Repair(SelfRepairOutcome::Skipped)
    ));
    assert_eq!(queued(&queue), 1, "a live job must not be duplicated");

    // The job exits without a fix, repeatedly: retried, but bounded by the cap.
    let mut dispatched = 1;
    for _ in 0..10 {
        queue.cancel_for_item(&item_id, |_| false).unwrap();
        if matches!(
            tick(&mcp, &queue, &item_id, &review, &labels),
            PassingPrOutcome::Repair(SelfRepairOutcome::Dispatched)
        ) {
            dispatched += 1;
        }
    }
    assert!(dispatched >= 2, "a lost job must be redispatched");
    assert_eq!(
        dispatched,
        crate::quota::decide::SELF_REPAIR_CAP,
        "retries stop at SELF_REPAIR_CAP"
    );
}

#[test]
fn approved_green_pr_stalled_on_threads_is_surfaced_once() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let approved = vec![PR_APPROVAL_LABEL.to_string()];
    let get_meta = || {
        mcp.with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap()
            .metadata
    };
    // Escalated threads hold the merge and no job will ever come.
    let held = ReviewBotState {
        blocking: 1,
        escalated: 1,
        ..Default::default()
    };
    let _ = take_pr_stage_comments_for_test();
    for _ in 0..REVIEW_STALL_TICK_LIMIT - 1 {
        tick(&mcp, &queue, &item_id, &held, &approved);
    }
    assert!(!get_meta().contains("review_stall_notified"));
    assert!(take_pr_stage_comments_for_test().is_empty());
    // Unapproved ticks do not count: the count resets.
    tick(&mcp, &queue, &item_id, &held, &[]);
    assert!(!get_meta().contains("review_stall"), "{}", get_meta());
    for _ in 0..REVIEW_STALL_TICK_LIMIT + 3 {
        tick(&mcp, &queue, &item_id, &held, &approved);
    }
    assert!(get_meta().contains("review_stall_notified"));
    let cards = take_pr_stage_comments_for_test();
    assert_eq!(cards.len(), 1, "surfaced exactly once: {cards:?}");
    assert!(cards[0].contains("stalled on review threads"));
    // Merely waiting on the bot is not a stall.
    let waiting = ReviewBotState {
        blocking: 1,
        ..Default::default()
    };
    tick(&mcp, &queue, &item_id, &waiting, &approved);
    assert!(!get_meta().contains("review_stall"));
}

#[test]
fn coderabbit_repair_posts_a_completion_summary_exactly_once() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let label_id_by_name = seed_gate_label(&mcp);
    let auth_conn = test_auth_conn();
    let findings = vec![coderabbit_finding(1, "coderabbitai[bot]")];

    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let outcome = coderabbit_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        &findings,
        &[],
        &label_id_by_name,
        "/repo",
    );
    assert!(matches!(outcome, SelfRepairOutcome::Dispatched));
    // Repair run reports what it fixed. Drain its queued job first so the
    // clean sweeps below aren't mistaken for in-flight work (closure is a
    // keep-filter: `false` cancels everything for the item).
    queue.cancel_for_item(&item_id, |_| false).unwrap();
    mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item_id.clone()),
        body: Some(format!(
            "{}\n\nFixed the empty-slice panic with checked indexing.",
            crate::dispatch_failure_ceiling::WORK_SUCCESS_MARKER
        )),
        ..Default::default()
    })
    .unwrap();

    for _ in 0..2 {
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap();
        let outcome = coderabbit_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            1,
            &[],
            &[CODERABBIT_REPAIR_PR_LABEL.to_string()],
            &label_id_by_name,
            "/repo",
        );
        assert!(matches!(outcome, SelfRepairOutcome::Skipped));
    }

    let comments = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id).unwrap())
        .unwrap();
    let summaries: Vec<_> = comments
        .iter()
        .filter(|c| c.body.starts_with(CODERABBIT_REPAIR_COMPLETE_MARKER))
        .collect();
    assert_eq!(summaries.len(), 1, "the summary must go out exactly once");
    assert!(
        summaries[0].body.contains("checked indexing"),
        "summary must quote what the repair run reported: {}",
        summaries[0].body
    );
}
