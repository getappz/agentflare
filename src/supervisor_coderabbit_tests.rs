//! CodeRabbit review-repair announce-once + completion-summary tests (item #633
//! follow-up). Split out of `supervisor_tests.rs`, which is frozen — do not
//! grow that file; add repair-cycle tests here instead.

use super::tests::{
    coderabbit_finding, seed_gate_label, seed_in_review_item_with_claim_age, test_auth_conn,
    test_mcp, test_queue,
};
use super::*;

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
fn coderabbit_repair_or_gate_redispatches_silently_for_identical_findings() {
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
        "the retry must still dispatch, just quietly"
    );
    assert_eq!(
        queue
            .list(Some(agentflare_jobs::JobState::Queued))
            .unwrap()
            .len(),
        1,
        "exactly the retry job must be queued (the cancelled prior attempt \
         no longer counts as queued)"
    );
    let comments = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id).unwrap())
        .unwrap();
    assert_eq!(
        comments
            .iter()
            .filter(|c| c.body.starts_with(CODERABBIT_REPAIR_MARKER))
            .count(),
        1,
        "identical findings must not post the announcement twice"
    );

    // New findings are a new announcement, not a silent retry.
    let mut evolved = findings.clone();
    evolved.push(coderabbit_finding(9, "coderabbitai[bot]"));
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
        &evolved,
        &[],
        &label_id_by_name,
        "/repo",
    );
    assert!(matches!(outcome, SelfRepairOutcome::Dispatched));
    let comments = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id).unwrap())
        .unwrap();
    assert_eq!(
        comments
            .iter()
            .filter(|c| c.body.starts_with(CODERABBIT_REPAIR_MARKER))
            .count(),
        2,
        "changed findings must announce again"
    );
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
