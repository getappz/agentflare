//! CodeRabbit review-repair announce-once + completion-summary tests (item #633
//! follow-up). Split out of `supervisor_tests.rs`, which is frozen — do not
//! grow that file; add repair-cycle tests here instead.

use super::tests::{
    claim_marker, coderabbit_finding, comment_json, seed_gate_label,
    seed_in_review_item_with_claim_age, test_auth_conn, test_mcp, test_queue,
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
            None,
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
        None,
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
        None,
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
    for _ in 0..REVIEW_STALL_TICK_LIMIT {
        tick(&mcp, &queue, &item_id, &held, &approved);
    }
    // The escalation put needs-human on the PR; later sweeps see it.
    let flagged = vec![
        PR_APPROVAL_LABEL.to_string(),
        NEEDS_HUMAN_PR_LABEL.to_string(),
    ];
    for _ in 0..3 {
        tick(&mcp, &queue, &item_id, &held, &flagged);
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

fn stall_item(mcp: &AgentflareMcp, item_id: &str) -> agentflare_backend::item::Item {
    mcp.with_backend_db(|conn| agentflare_backend::item::get(conn, item_id).unwrap())
        .unwrap()
}

fn held_state() -> ReviewBotState {
    ReviewBotState {
        blocking: 1,
        escalated: 1,
        ..Default::default()
    }
}

#[test]
fn notified_stall_keeps_its_label_and_does_not_renotify() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let approved = vec![PR_APPROVAL_LABEL.to_string()];
    let _ = take_pr_stage_comments_for_test();
    for _ in 0..REVIEW_STALL_TICK_LIMIT {
        tick(&mcp, &queue, &item_id, &held_state(), &approved);
    }
    assert_eq!(take_pr_stage_comments_for_test().len(), 1);
    // Label now on the PR: further stalled ticks are a quiet no-op that keeps
    // the notified state, and handle_ci_green must not revert the label.
    let flagged = vec![
        PR_APPROVAL_LABEL.to_string(),
        NEEDS_HUMAN_PR_LABEL.to_string(),
    ];
    for _ in 0..REVIEW_STALL_TICK_LIMIT * 2 {
        tick(&mcp, &queue, &item_id, &held_state(), &flagged);
    }
    assert!(take_pr_stage_comments_for_test().is_empty());
    let item = stall_item(&mcp, &item_id);
    assert!(review_stall_notified(&mcp, &item));
    assert!(keeps_needs_human_label(&mcp, &item, &flagged));
    // Without a stall notification the label is still reverted as before.
    let other = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    assert!(!keeps_needs_human_label(
        &mcp,
        &stall_item(&mcp, &other),
        &flagged
    ));
}

#[test]
fn failed_label_add_is_retried_and_dismissed_label_re_escalates() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let approved = vec![PR_APPROVAL_LABEL.to_string()];
    let _ = take_pr_stage_comments_for_test();
    // The label add fails: the marker must not be recorded, so it retries.
    set_pr_stage_label_add_fails_for_test(true);
    for _ in 0..REVIEW_STALL_TICK_LIMIT {
        tick(&mcp, &queue, &item_id, &held_state(), &approved);
    }
    assert!(!review_stall_notified(&mcp, &stall_item(&mcp, &item_id)));
    set_pr_stage_label_add_fails_for_test(false);
    tick(&mcp, &queue, &item_id, &held_state(), &approved);
    assert!(review_stall_notified(&mcp, &stall_item(&mcp, &item_id)));
    // A human removes the label while the stall persists: quiet for a fresh
    // window, then escalated again (not silently stalled forever).
    let _ = take_pr_stage_comments_for_test();
    for _ in 0..REVIEW_STALL_TICK_LIMIT - 1 {
        tick(&mcp, &queue, &item_id, &held_state(), &approved);
    }
    assert!(take_pr_stage_comments_for_test().is_empty());
    tick(&mcp, &queue, &item_id, &held_state(), &approved);
    assert_eq!(take_pr_stage_comments_for_test().len(), 1);
    assert!(review_stall_notified(&mcp, &stall_item(&mcp, &item_id)));
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
        None,
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
            None,
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

#[test]
fn void_coderabbit_repair_that_exits_cleanly_counts_toward_the_cap() {
    const HEAD: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let labels = vec![PR_APPROVAL_LABEL.to_string()];
    let review = ReviewBotState::for_dispatch(vec![coderabbit_finding(1, "coderabbitai[bot]")]);
    let label_id_by_name = seed_gate_label(&mcp);

    assert!(matches!(
        tick_with_head(&mcp, &queue, &item_id, &review, &labels, HEAD),
        PassingPrOutcome::Repair(SelfRepairOutcome::Dispatched)
    ));
    queue.cancel_for_item(&item_id, |_| false).unwrap();
    mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item_id.clone()),
        body: Some(format!(
            "{}

No-op repair run.",
            crate::dispatch_failure_ceiling::WORK_SUCCESS_MARKER
        )),
        ..Default::default()
    })
    .unwrap();

    for round in 0..crate::quota::decide::SELF_REPAIR_CAP {
        if round > 0 {
            mcp.comment_impl(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.clone()),
                body: Some(format!(
                    "{}

No-op repair run #{round}.",
                    crate::dispatch_failure_ceiling::WORK_SUCCESS_MARKER
                )),
                ..Default::default()
            })
            .unwrap();
            queue.cancel_for_item(&item_id, |_| false).unwrap();
        }
        let item = mcp
            .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap();
        maybe_credit_void_coderabbit_repair(
            &mcp,
            &item,
            Some(HEAD),
            false,
            true,
            &review.to_dispatch,
        );
        if coderabbit_repair_capped_unresolved(&mcp, &item) {
            break;
        }
        let _ = tick_with_head(&mcp, &queue, &item_id, &review, &labels, HEAD);
    }
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let _ = coderabbit_repair_or_gate(
        &mcp,
        &queue,
        &test_auth_conn(),
        agentflare_resource_gate::Policy::Normal,
        &item,
        42,
        &review.to_dispatch,
        &labels,
        &label_id_by_name,
        "/repo",
        Some(HEAD),
    );
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    assert!(coderabbit_repair_capped_unresolved(&mcp, &item));
    let labels_on_item = mcp
        .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
        .unwrap();
    assert!(labels_on_item.contains(&label_id_by_name[NEEDS_HUMAN_GATE_LABEL]));
}

fn tick_with_head(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    item_id: &str,
    review: &ReviewBotState,
    labels: &[String],
    head: &str,
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
            head_sha: Some(head),
            auto_merge: &auto,
        },
    )
}

#[test]
fn claim_self_repair_does_not_block_redispatch_after_its_own_job_exited() {
    // PR 931: our own "Claiming self-repair" marker is still live (job exited
    // without a fix). Redispatch must proceed -- true, with no second marker
    // posted -- rather than deferring on the owner's own stale claim.
    let me = crate::github::bridge::config::machine_label();
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(
            200,
            &comment_json(1, &claim_marker(&me, 1_000)),
        ),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };
    assert!(claim_self_repair(&client, &repo, 931, "i1", 1_050));
    assert_eq!(server.requests().len(), 1, "no extra claim posted");
}
