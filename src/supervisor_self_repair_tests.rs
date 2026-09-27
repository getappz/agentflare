//! CI-self-repair and merge-conflict-repair announce-once + completion-summary
//! tests (item #303: `self_repair_or_gate`'s retry cap didn't work because
//! `list_by_item`-based comment counting alone stayed at 0 across hundreds of
//! real dispatches on items #300/#54). Mirrors `supervisor_coderabbit_tests.rs`
//! (item #633), which fixed the exact same class of bug for
//! `coderabbit_repair_or_gate` first. Split out rather than added to
//! `supervisor_tests.rs`, which is frozen -- see that file's own doc comment.

use super::tests::{
    seed_gate_label, seed_in_review_item_with_claim_age, test_auth_conn, test_mcp, test_queue,
};
use super::*;

fn current_item(mcp: &AgentflareMcp, item_id: &str) -> agentflare_backend::item::Item {
    mcp.with_backend_db(|conn| agentflare_backend::item::get(conn, item_id).unwrap())
        .unwrap()
}

#[test]
fn repair_trigger_fingerprint_ignores_check_order_but_not_content() {
    let a = RepairTrigger::FailingChecks(&["clippy".to_string(), "fmt".to_string()]);
    let b = RepairTrigger::FailingChecks(&["fmt".to_string(), "clippy".to_string()]);
    assert_eq!(
        a.fingerprint(),
        b.fingerprint(),
        "reordered check lists must not re-announce"
    );
    let changed = RepairTrigger::FailingChecks(&["clippy".to_string(), "test".to_string()]);
    assert_ne!(
        a.fingerprint(),
        changed.fingerprint(),
        "a different failing-check set must re-announce"
    );
    // MergeConflict has exactly one state, so its own fingerprint is stable
    // across constructions and distinct from the CI-checks fingerprint.
    assert_eq!(
        RepairTrigger::MergeConflict.fingerprint(),
        RepairTrigger::MergeConflict.fingerprint()
    );
    assert_ne!(RepairTrigger::MergeConflict.fingerprint(), a.fingerprint());
}

/// Item #303's core bug: on items #300/#54, `list_by_item`-based marker
/// counting stayed at 0 across hundreds of dispatches, so the cap never
/// tripped -- 182 and 190 identical dispatch comments respectively. This
/// pins the fix's actual guarantee: even calling `self_repair_or_gate`
/// `SELF_REPAIR_CAP + 5` times with the *same* failing-check fingerprint
/// (the retry-storm scenario) posts exactly one marker comment, then exactly
/// one cap-reached comment, and never anything more -- the metadata silent
/// counter trips the cap independently of comment counting.
#[test]
fn self_repair_or_gate_caps_after_a_bounded_number_of_silent_retries() {
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
        let label_id_by_name = seed_gate_label(&mcp);
        let auth_conn = test_auth_conn();
        let checks = vec!["clippy".to_string()];

        for i in 0..(crate::quota::decide::SELF_REPAIR_CAP + 5) {
            let item = current_item(&mcp, &item_id);
            let outcome = self_repair_or_gate(
                &mcp,
                &queue,
                &auth_conn,
                agentflare_resource_gate::Policy::Normal,
                &item,
                1,
                RepairTrigger::FailingChecks(&checks),
                &[],
                &label_id_by_name,
                "/repo",
            );
            // Cancel whatever this attempt queued so the next call doesn't
            // see it as still in flight (mirrors the CodeRabbit test's own
            // per-round `cancel_for_item`).
            queue.cancel_for_item(&item_id, |_| false).unwrap();

            if i < crate::quota::decide::SELF_REPAIR_CAP {
                assert!(
                    matches!(outcome, SelfRepairOutcome::Dispatched),
                    "attempt {i} (below the cap) must still dispatch"
                );
            } else {
                assert!(
                    matches!(outcome, SelfRepairOutcome::Skipped),
                    "attempt {i} (at/above the cap) must gate instead of dispatching again"
                );
            }
        }

        let comments = mcp
            .with_backend_db(|conn| {
                agentflare_backend::comment::list_by_item(conn, &item_id).unwrap()
            })
            .unwrap();
        assert_eq!(
            comments
                .iter()
                .filter(|c| c.body.starts_with(CI_SELF_REPAIR_MARKER))
                .count(),
            1,
            "identical failing checks must post the dispatch announcement exactly once, \
             never once per retry"
        );
        assert_eq!(
            comments
                .iter()
                .filter(|c| c.body.contains("CI self-repair cap reached"))
                .count(),
            1,
            "the cap-reached comment must also post exactly once, not once per call past the cap"
        );

        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(
            labels.contains(&label_id_by_name[NEEDS_HUMAN_GATE_LABEL]),
            "cap reached must gate the item for a human"
        );
    });
}

/// PR #818 review finding: `restore_after_terminal_failure` adds
/// `NEEDS_MANUAL_LABEL` once its own dispatch-failure cap trips, but this
/// guard used to only check `NEEDS_HUMAN_GATE_LABEL` -- so a PR that had
/// just hit that cap could still enter self-repair on the very next sweep
/// tick, defeating it.
#[test]
fn self_repair_or_gate_skips_when_needs_manual_dispatch_label_is_present() {
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
        let mut label_id_by_name = seed_gate_label(&mcp);
        let auth_conn = test_auth_conn();
        let checks = vec!["clippy".to_string()];

        let manual_id = mcp
            .with_backend_db(|conn| {
                let project = mcp.resolve_project(conn).unwrap();
                agentflare_backend::label::create(
                    conn,
                    agentflare_backend::label::CreateLabel {
                        project_id: Some(project.id.clone()),
                        workspace_id: project.workspace_id.clone(),
                        name: NEEDS_MANUAL_LABEL.into(),
                        color: None,
                        parent_id: None,
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                    },
                )
                .unwrap()
                .id
            })
            .unwrap();
        label_id_by_name.insert(NEEDS_MANUAL_LABEL.to_string(), manual_id.clone());
        mcp.with_backend_db(|conn| {
            agentflare_backend::item::add_label(conn, &item_id, &manual_id).unwrap()
        })
        .unwrap();

        let item = current_item(&mcp, &item_id);
        let outcome = self_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            1,
            RepairTrigger::FailingChecks(&checks),
            &[],
            &label_id_by_name,
            "/repo",
        );
        assert!(
            matches!(outcome, SelfRepairOutcome::Skipped),
            "a PR that just hit the dispatch-failure cap (needs-manual-dispatch) must not \
             re-enter self-repair on the next sweep tick"
        );
    });
}

#[test]
fn self_repair_or_gate_redispatches_silently_for_identical_failing_checks() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let label_id_by_name = seed_gate_label(&mcp);
    let auth_conn = test_auth_conn();
    let checks = vec!["clippy".to_string()];

    let item = current_item(&mcp, &item_id);
    let outcome = self_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        RepairTrigger::FailingChecks(&checks),
        &[],
        &label_id_by_name,
        "/repo",
    );
    assert!(matches!(outcome, SelfRepairOutcome::Dispatched));
    queue.cancel_for_item(&item_id, |_| false).unwrap();

    let item = current_item(&mcp, &item_id);
    let outcome = self_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        RepairTrigger::FailingChecks(&checks),
        &[],
        &label_id_by_name,
        "/repo",
    );
    assert!(
        matches!(outcome, SelfRepairOutcome::Dispatched),
        "the retry must still dispatch, just quietly"
    );

    let comments = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id).unwrap())
        .unwrap();
    assert_eq!(
        comments
            .iter()
            .filter(|c| c.body.starts_with(CI_SELF_REPAIR_MARKER))
            .count(),
        1,
        "identical failing checks must not post the announcement twice"
    );

    // A genuinely different failing-check set is a new announcement.
    let evolved = vec!["clippy".to_string(), "fmt".to_string()];
    queue.cancel_for_item(&item_id, |_| false).unwrap();
    let item = current_item(&mcp, &item_id);
    let outcome = self_repair_or_gate(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        1,
        RepairTrigger::FailingChecks(&evolved),
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
            .filter(|c| c.body.starts_with(CI_SELF_REPAIR_MARKER))
            .count(),
        2,
        "a changed failing-check set must announce again"
    );
}

#[test]
fn self_repair_or_gate_redispatches_silently_for_a_persistent_merge_conflict() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let label_id_by_name = seed_gate_label(&mcp);
    let auth_conn = test_auth_conn();

    for _ in 0..2 {
        let item = current_item(&mcp, &item_id);
        let outcome = self_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            1,
            RepairTrigger::MergeConflict,
            &[],
            &label_id_by_name,
            "/repo",
        );
        assert!(matches!(outcome, SelfRepairOutcome::Dispatched));
        queue.cancel_for_item(&item_id, |_| false).unwrap();
    }

    let comments = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id).unwrap())
        .unwrap();
    assert_eq!(
        comments
            .iter()
            .filter(|c| c.body.starts_with(CONFLICT_REPAIR_MARKER))
            .count(),
        1,
        "a still-unresolved merge conflict must not re-announce on every retry"
    );
}

#[test]
fn ci_self_repair_completion_summary_posts_exactly_once() {
    let mcp = test_mcp();
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);

    // Nothing announced yet -- no summary to post.
    let item = current_item(&mcp, &item_id);
    assert_eq!(
        maybe_post_repair_complete_summary(
            &mcp,
            &item,
            CI_SELF_REPAIR_MARKER,
            CI_SELF_REPAIR_COMPLETE_MARKER,
            "CI checks are passing again",
            CI_SELF_REPAIR_ANNOUNCED_KEY,
            CI_SELF_REPAIR_SILENT_KEY,
            CI_SELF_REPAIR_COMPLETED_KEY,
        ),
        None,
        "nothing was ever announced, so there is nothing to complete"
    );

    // Announce a dispatch (mirrors what `self_repair_or_gate` records), then
    // simulate the repair run reporting what it fixed.
    persist_repair_track(
        &mcp,
        &item_id,
        CI_SELF_REPAIR_ANNOUNCED_KEY,
        CI_SELF_REPAIR_SILENT_KEY,
        CI_SELF_REPAIR_COMPLETED_KEY,
        Some("clippy"),
        false,
        None,
    );
    mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item_id.clone()),
        body: Some(format!(
            "{CI_SELF_REPAIR_MARKER}\n\nFailing checks: clippy."
        )),
        ..Default::default()
    })
    .unwrap();
    mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item_id.clone()),
        body: Some(format!(
            "{}\n\nFixed the clippy lint.",
            crate::dispatch_failure_ceiling::WORK_SUCCESS_MARKER
        )),
        ..Default::default()
    })
    .unwrap();

    for _ in 0..2 {
        let item = current_item(&mcp, &item_id);
        maybe_post_repair_complete_summary(
            &mcp,
            &item,
            CI_SELF_REPAIR_MARKER,
            CI_SELF_REPAIR_COMPLETE_MARKER,
            "CI checks are passing again",
            CI_SELF_REPAIR_ANNOUNCED_KEY,
            CI_SELF_REPAIR_SILENT_KEY,
            CI_SELF_REPAIR_COMPLETED_KEY,
        );
    }

    let comments = mcp
        .with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, &item_id).unwrap())
        .unwrap();
    let summaries: Vec<_> = comments
        .iter()
        .filter(|c| c.body.starts_with(CI_SELF_REPAIR_COMPLETE_MARKER))
        .collect();
    assert_eq!(summaries.len(), 1, "the summary must go out exactly once");
    assert!(
        summaries[0].body.contains("clippy lint"),
        "summary must quote what the repair run reported: {}",
        summaries[0].body
    );
}
