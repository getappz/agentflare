//! CI-self-repair and merge-conflict-repair announce-once + completion-summary
//! tests (item #303: `self_repair_or_gate`'s retry cap didn't work because
//! `list_by_item`-based comment counting alone stayed at 0 across hundreds of
//! real dispatches on items #300/#54). Mirrors `supervisor_coderabbit_tests.rs`
//! (item #633), which fixed the exact same class of bug for
//! `coderabbit_repair_or_gate` first. Split out rather than added to
//! `supervisor_tests.rs`, which is frozen -- see that file's own doc comment.

use std::path::Path;

use super::tests::{
    coderabbit_finding, repair_pr, seed_gate_label, seed_in_review_item_with_claim_age,
    test_auth_conn, test_mcp, test_queue,
};
use super::*;
use crate::worktree::AutoMergeRef;

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
                Some(&repair_pr(&item, 1)),
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
            Some(&repair_pr(&item, 1)),
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

/// Item #341: a branch held by another item's worktree defers repair -- but
/// only below the cap. An item that already exhausted its attempts must still
/// reach the cap handling (comment + human gate) while the branch stays
/// occupied, not defer forever.
#[test]
fn self_repair_or_gate_defers_a_branch_collision_only_below_the_cap() {
    crate::paths::test_support::with_temp_home(|| {
        let git = |dir: &Path, args: &[&str]| {
            let ok = std::process::Command::new("git")
                .current_dir(dir)
                .args(["-c", "user.name=t", "-c", "user.email=t@t"])
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?} failed");
        };
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init", "-q", "-b", "main"]);
        git(
            repo.path(),
            &["commit", "-q", "--allow-empty", "-m", "init"],
        );
        let foreign = repo.path().join("foreign");
        git(
            repo.path(),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat/held",
                foreign.to_str().unwrap(),
            ],
        );
        let folder = repo.path().to_str().unwrap();

        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
        mcp.with_backend_db(|conn| {
            crate::mcp_server::merge_item_metadata(conn, &item_id, |m| {
                m.insert(
                    "pr".into(),
                    serde_json::json!({"number": 1, "branch": "feat/held"}),
                );
            })
        })
        .unwrap()
        .unwrap();
        let label_id_by_name = seed_gate_label(&mcp);
        let auth_conn = test_auth_conn();
        let checks = vec!["clippy".to_string()];
        let run = || {
            let item = current_item(&mcp, &item_id);
            self_repair_or_gate(
                &mcp,
                &queue,
                &auth_conn,
                agentflare_resource_gate::Policy::Normal,
                &item,
                Some(&repair_pr(&item, 1)),
                RepairTrigger::FailingChecks(&checks),
                &[],
                &label_id_by_name,
                folder,
            )
        };

        assert!(
            matches!(run(), SelfRepairOutcome::Deferred),
            "below the cap, a collision defers without enqueueing a doomed job"
        );

        for _ in 0..crate::quota::decide::SELF_REPAIR_CAP {
            mcp.comment_impl(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.clone()),
                body: Some(format!("{CI_SELF_REPAIR_MARKER}\n\njob: prior")),
                ..Default::default()
            })
            .unwrap();
        }
        assert!(
            matches!(run(), SelfRepairOutcome::Skipped),
            "at the cap, the item must reach cap handling, not defer on the collision"
        );
        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(
            labels.contains(&label_id_by_name[NEEDS_HUMAN_GATE_LABEL]),
            "the capped item must still be gated for a human"
        );
    });
}

/// Item #341: a green, approved PR whose item is gated used to be skipped in
/// silence; the sweep now raises the operator notice once (and only for an
/// approved PR -- an unapproved one already has its own approval card).
#[test]
fn handle_ci_green_notifies_once_for_a_gated_approved_pr() {
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        let auth_conn = test_auth_conn();
        let auto_merge = AutoMergeRef::default();
        let mut label_id_by_name = seed_gate_label(&mcp);
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

        let sweep_once = |approved: bool| {
            let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
            mcp.with_backend_db(|conn| {
                agentflare_backend::item::add_label(conn, &item_id, &manual_id).unwrap()
            })
            .unwrap();
            let item = current_item(&mcp, &item_id);
            let labels = if approved {
                vec![PR_APPROVAL_LABEL.to_string()]
            } else {
                vec![]
            };
            let mut sweep = ReviewSweepResult {
                promoted: 0,
                self_repaired: 0,
                review_repaired: 0,
                skipped: 0,
                waiting: 0,
                updated: 0,
                discovered: 0,
                requeued: 0,
                stray_claimed: 0,
            };
            handle_ci_green(
                &mcp,
                &queue,
                &auth_conn,
                agentflare_resource_gate::Policy::Normal,
                &item,
                1,
                &labels,
                CiGreenMerge::Allowed {
                    head_sha: None,
                    auto_merge: &auto_merge,
                },
                &label_id_by_name,
                "/repo",
                Path::new("/repo"),
                &mut sweep,
            );
            assert_eq!(sweep.skipped, 1, "a gated PR is skipped, not merged");
            // `first_time_gated` is false once the notice key was consumed.
            first_time_gated(&format!("pr-gated:{}", item.id))
        };

        assert!(
            sweep_once(false),
            "an unapproved PR must not consume the gated-PR notice"
        );
        assert!(
            !sweep_once(true),
            "an approved, gated PR must have raised its notice"
        );
    });
}

#[test]
fn merge_or_repair_findings_clears_dispatch_gate_labels_when_approved_and_clean() {
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        let auth_conn = test_auth_conn();
        let auto_merge = AutoMergeRef::default();
        let mut label_id_by_name = seed_gate_label(&mcp);
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

        let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
        let gate_id = label_id_by_name[NEEDS_HUMAN_GATE_LABEL].clone();
        mcp.with_backend_db(|conn| {
            agentflare_backend::item::add_label(conn, &item_id, &manual_id).unwrap();
            agentflare_backend::item::add_label(conn, &item_id, &gate_id).unwrap();
        })
        .unwrap();

        let item = current_item(&mcp, &item_id);
        let labels = vec![PR_APPROVAL_LABEL.to_string()];
        let review = ReviewBotState::default();

        let _ = merge_or_repair_findings(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            Path::new("/repo"),
            1,
            &review,
            &labels,
            &label_id_by_name,
            "/repo",
            CiGreenMerge::Allowed {
                head_sha: None,
                auto_merge: &auto_merge,
            },
        );

        let remaining = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(
            !remaining.contains(&manual_id),
            "needs-manual-dispatch must be cleared on approved green PR"
        );
        assert!(
            !remaining.contains(&gate_id),
            "needs-human-gate must be cleared on approved green PR"
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
        Some(&repair_pr(&item, 1)),
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
        Some(&repair_pr(&item, 1)),
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
        Some(&repair_pr(&item, 1)),
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
            Some(&repair_pr(&item, 1)),
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

/// PR #829: a capped CodeRabbit repair on a CI-green PR must not alternate
/// `NEEDS_HUMAN_PR_LABEL` with `IN_REVIEW_PR_LABEL` (and cap / CI-green
/// comments) on every supervisor sweep.
#[test]
fn coderabbit_cap_and_ci_green_do_not_flip_flop_across_sweeps() {
    crate::paths::test_support::with_temp_home(|| {
        let _ = take_pr_stage_comments_for_test();
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
        let label_id_by_name = seed_gate_label(&mcp);
        let auth_conn = test_auth_conn();
        let findings = vec![coderabbit_finding(1, "coderabbitai[bot]")];
        let fingerprint = coderabbit_findings_fingerprint(&findings);
        let repo_root = Path::new("/repo");
        let auto_merge = AutoMergeRef::default();

        for _ in 0..crate::quota::decide::SELF_REPAIR_CAP {
            mcp.comment_impl(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.clone()),
                body: Some(format!("{CODERABBIT_REPAIR_MARKER}\n\njob: prior")),
                ..Default::default()
            })
            .unwrap();
        }
        persist_repair_track(
            &mcp,
            &item_id,
            CODERABBIT_REPAIR_ANNOUNCED_KEY,
            CODERABBIT_REPAIR_SILENT_KEY,
            CODERABBIT_REPAIR_COMPLETED_KEY,
            Some(&fingerprint),
            false,
            None,
        );

        let item = current_item(&mcp, &item_id);
        assert!(coderabbit_repair_capped_unresolved(&mcp, &item));

        // First cap announcement (PR not yet carrying needs-human).
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
        assert!(matches!(outcome, SelfRepairOutcome::Skipped));

        let labels_on_item = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(
            labels_on_item.contains(&label_id_by_name[NEEDS_HUMAN_GATE_LABEL]),
            "cap must always land the item gate label"
        );

        let pr_needs_human = vec![NEEDS_HUMAN_PR_LABEL.to_string()];
        assert!(
            ci_green_stale_stage_label(&pr_needs_human, true).is_none(),
            "CI green must not strip needs-human while CodeRabbit repair is capped"
        );

        // The cap side runs against a label map with no gate label, as on a
        // project that has none: the item never reads as already gated, so
        // every sweep re-enters the cap branch -- the state PR #829 looped
        // in. Odd sweeps drop the PR's stage label too, so the fingerprint
        // guard is what has to hold, not only the label one.
        let ungated = std::collections::HashMap::new();
        let pr_in_review = vec![IN_REVIEW_PR_LABEL.to_string()];
        const SWEEPS: usize = 8;
        for sweep_no in 0..SWEEPS {
            let item = current_item(&mcp, &item_id);
            let mut sweep = ReviewSweepResult {
                promoted: 0,
                self_repaired: 0,
                review_repaired: 0,
                skipped: 0,
                waiting: 0,
                updated: 0,
                discovered: 0,
                requeued: 0,
                stray_claimed: 0,
            };
            handle_ci_green(
                &mcp,
                &queue,
                &auth_conn,
                agentflare_resource_gate::Policy::Normal,
                &item,
                1,
                &pr_needs_human,
                CiGreenMerge::Allowed {
                    head_sha: None,
                    auto_merge: &auto_merge,
                },
                &label_id_by_name,
                "/repo",
                repo_root,
                &mut sweep,
            );
            let outcome = coderabbit_repair_or_gate(
                &mcp,
                &queue,
                &auth_conn,
                agentflare_resource_gate::Policy::Normal,
                &item,
                1,
                &findings,
                if sweep_no % 2 == 0 {
                    &pr_needs_human
                } else {
                    &pr_in_review
                },
                &ungated,
                "/repo",
                None,
            );
            assert!(matches!(outcome, SelfRepairOutcome::Skipped));
        }

        let pr_stage_comments = take_pr_stage_comments_for_test();
        assert_eq!(
            pr_stage_comments
                .iter()
                .filter(|c| c.contains("CI green"))
                .count(),
            0,
            "handle_ci_green must not post CI-green PR comments while capped"
        );

        let comments = mcp
            .with_backend_db(|conn| {
                agentflare_backend::comment::list_by_item(conn, &item_id).unwrap()
            })
            .unwrap();
        assert_eq!(
            comments
                .iter()
                .filter(|c| c.body.starts_with(CODERABBIT_REPAIR_CAP_MARKER))
                .count(),
            1,
            "cap reached must be announced on the item exactly once across sweeps"
        );
    });
}

#[test]
fn ci_green_stale_stage_label_ignores_self_repair_while_capped_with_needs_human() {
    let labels = vec![
        SELF_REPAIR_PR_LABEL.to_string(),
        NEEDS_HUMAN_PR_LABEL.to_string(),
    ];
    assert!(
        ci_green_stale_stage_label(&labels, true).is_none(),
        "must not revert self-repair or needs-human while CodeRabbit cap + needs-human gate"
    );
}

#[test]
fn coderabbit_capped_unresolved_when_new_cap_fingerprint_differs_from_completed() {
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
        let findings_a = vec![coderabbit_finding(1, "coderabbitai[bot]")];
        let findings_b = vec![coderabbit_finding(2, "coderabbitai[bot]")];
        let fp_a = coderabbit_findings_fingerprint(&findings_a);
        let fp_b = coderabbit_findings_fingerprint(&findings_b);

        for _ in 0..crate::quota::decide::SELF_REPAIR_CAP {
            mcp.comment_impl(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.clone()),
                body: Some(format!("{CODERABBIT_REPAIR_MARKER}\n\njob: prior")),
                ..Default::default()
            })
            .unwrap();
        }
        persist_repair_track(
            &mcp,
            &item_id,
            CODERABBIT_REPAIR_ANNOUNCED_KEY,
            CODERABBIT_REPAIR_SILENT_KEY,
            CODERABBIT_REPAIR_COMPLETED_KEY,
            Some(&fp_a),
            false,
            Some(&fp_a),
        );
        mcp.with_backend_db(|conn| {
            crate::mcp_server::merge_item_metadata(conn, &item_id, |m| {
                m.insert(CODERABBIT_REPAIR_CAP_KEY.into(), fp_b.clone().into());
            })
            .unwrap()
        })
        .unwrap();

        let item = current_item(&mcp, &item_id);
        assert!(
            coderabbit_repair_capped_unresolved(&mcp, &item),
            "new capped findings must stay unresolved even when an earlier fingerprint completed"
        );

        // ...and the cap closes once the findings clear, though `fp_b` was
        // never the announced fingerprint a completion is recorded against.
        let outcome = coderabbit_repair_or_gate(
            &mcp,
            &test_queue(),
            &test_auth_conn(),
            agentflare_resource_gate::Policy::Normal,
            &item,
            1,
            &[],
            &[],
            &seed_gate_label(&mcp),
            "/repo",
            None,
        );
        assert!(matches!(outcome, SelfRepairOutcome::Skipped));
        let item = current_item(&mcp, &item_id);
        assert!(
            !coderabbit_repair_capped_unresolved(&mcp, &item),
            "a cap whose findings are all resolved must stop holding needs-human"
        );
    });
}

#[test]
fn coderabbit_cap_announced_requires_exact_fingerprint_not_substring() {
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
        let label_id_by_name = seed_gate_label(&mcp);
        let findings_a = vec![coderabbit_finding(1, "coderabbitai[bot]")];
        let mut second = coderabbit_finding(2, "coderabbitai[bot]");
        second.body = "Different finding body for fingerprint split.".into();
        let findings_ab = vec![findings_a[0].clone(), second];
        let fp_ab = coderabbit_findings_fingerprint(&findings_ab);
        let fp_a = coderabbit_findings_fingerprint(&findings_a);

        for _ in 0..crate::quota::decide::SELF_REPAIR_CAP {
            mcp.comment_impl(CommentRequest {
                action: "create".into(),
                item_id: Some(item_id.clone()),
                body: Some(format!("{CODERABBIT_REPAIR_MARKER}\n\njob: prior")),
                ..Default::default()
            })
            .unwrap();
        }
        persist_repair_track(
            &mcp,
            &item_id,
            CODERABBIT_REPAIR_ANNOUNCED_KEY,
            CODERABBIT_REPAIR_SILENT_KEY,
            CODERABBIT_REPAIR_COMPLETED_KEY,
            Some(&fp_ab),
            false,
            None,
        );
        mcp.with_backend_db(|conn| {
            crate::mcp_server::merge_item_metadata(conn, &item_id, |m| {
                m.insert(CODERABBIT_REPAIR_CAP_KEY.into(), fp_ab.clone().into());
            })
            .unwrap()
        })
        .unwrap();
        mcp.comment_impl(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.clone()),
            body: Some(format!(
                "{CODERABBIT_REPAIR_CAP_MARKER}\n\nfindings fingerprint: {fp_ab}\n\nprior cap"
            )),
            ..Default::default()
        })
        .unwrap();

        let item = current_item(&mcp, &item_id);
        let comments_before = mcp
            .with_backend_db(|conn| {
                agentflare_backend::comment::list_by_item(conn, &item_id).unwrap()
            })
            .unwrap();
        let cap_before = comments_before
            .iter()
            .filter(|c| c.body.starts_with(CODERABBIT_REPAIR_CAP_MARKER))
            .count();

        let outcome = coderabbit_repair_or_gate(
            &mcp,
            &queue,
            &test_auth_conn(),
            agentflare_resource_gate::Policy::Normal,
            &item,
            1,
            &findings_a,
            &[],
            &label_id_by_name,
            "/repo",
            None,
        );
        assert!(matches!(outcome, SelfRepairOutcome::Skipped));

        let comments_after = mcp
            .with_backend_db(|conn| {
                agentflare_backend::comment::list_by_item(conn, &item_id).unwrap()
            })
            .unwrap();
        let cap_after = comments_after
            .iter()
            .filter(|c| c.body.starts_with(CODERABBIT_REPAIR_CAP_MARKER))
            .count();
        assert_eq!(
            cap_after,
            cap_before + 1,
            "fingerprint {fp_a} is not the announced cap {fp_ab}, so a new cap comment must post"
        );
        assert!(
            comments_after.iter().any(|c| {
                c.body.starts_with(CODERABBIT_REPAIR_CAP_MARKER)
                    && c.body.contains(&format!("findings fingerprint: {fp_a}"))
            }),
            "new cap must reference the current findings fingerprint exactly"
        );
    });
}

#[test]
fn repair_agent_falls_back_to_the_last_work_job_even_behind_100_newer_jobs() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, None, 1_900);
    let mut item = current_item(&mcp, &item_id);
    item.assignee_agent = None;

    let work = |id: &str, agent: &str| {
        agentflare_jobs::AgentJob::new("agentflare-work")
            .args(vec![id.to_string(), agent.to_string()])
            .in_process()
    };
    queue.enqueue(&work(&item_id, "claude-code")).unwrap();
    for i in 0..105 {
        queue
            .enqueue(&work(&format!("other-{i}"), "opencode"))
            .unwrap();
    }
    let last = queue
        .last_agent_for_item("agentflare-work", &item_id)
        .unwrap();
    assert_eq!(last.as_deref(), Some("claude-code"));
    let agent = resolve_repair_agent_with(&item, last.as_deref(), || {
        panic!("job must win over fallback")
    });
    assert_eq!(agent, Some(agent_registry::Agent::ClaudeCode));
}

#[test]
fn repair_agent_prefers_the_agent_the_executor_failed_over_to() {
    let mcp = test_mcp();
    let queue = test_queue();
    let item_id = seed_in_review_item_with_claim_age(&mcp, None, 1_900);
    let mut item = current_item(&mcp, &item_id);
    item.assignee_agent = None;

    let job = agentflare_jobs::AgentJob::new("agentflare-work")
        .args(vec![item_id.clone(), "claude-code".to_string()])
        .in_process();
    let job_id = queue.enqueue(&job).unwrap().id;
    // The worker wires this sink; the executor's pre-launch failover from
    // `claude-code` to `opencode` goes through `settle_execution_agent`.
    let sink_queue = queue.clone();
    let sink_id = job_id.clone();
    let _guard = agentflare_jobs::cancel::register(&job_id, || false)
        .with_agent_sink(move |a| sink_queue.set_effective_agent(&sink_id, a).unwrap());
    let (ran, model) = crate::cli::work::settle_execution_agent(
        &job_id,
        "claude-code".to_string(),
        Some("pinned".to_string()),
        |from| {
            assert_eq!(from, "claude-code");
            Some("opencode".to_string())
        },
    );
    assert_eq!((ran.as_str(), model), ("opencode", None));

    let last = queue
        .last_agent_for_item("agentflare-work", &item_id)
        .unwrap();
    assert_eq!(last.as_deref(), Some("opencode"));
    let agent = resolve_repair_agent_with(&item, last.as_deref(), || None);
    assert_eq!(agent, Some(agent_registry::Agent::Opencode));
}

#[test]
fn repair_agent_uses_the_sole_installed_agent_with_an_empty_router() {
    let mcp = test_mcp();
    let item_id = seed_in_review_item_with_claim_age(&mcp, None, 1_900);
    let mut item = current_item(&mcp, &item_id);
    item.assignee_agent = None;
    let config = agent_registry::RouterConfig::default();
    let installed = [agent_registry::Agent::Opencode];
    let mut rotation = std::collections::HashMap::new();

    let agent = resolve_repair_agent_with(&item, None, || {
        sole_or_routed_agent(&item, &config, &installed, &mut rotation)
    });
    assert_eq!(agent, Some(agent_registry::Agent::Opencode));
}

#[test]
fn repair_agent_is_none_when_nothing_resolves() {
    let mcp = test_mcp();
    let item_id = seed_in_review_item_with_claim_age(&mcp, None, 1_900);
    let mut item = current_item(&mcp, &item_id);
    item.assignee_agent = None;
    let config = agent_registry::RouterConfig::default();
    let installed = [
        agent_registry::Agent::Opencode,
        agent_registry::Agent::ClaudeCode,
    ];
    let mut rotation = std::collections::HashMap::new();

    let agent = resolve_repair_agent_with(&item, None, || {
        sole_or_routed_agent(&item, &config, &installed, &mut rotation)
    });
    assert_eq!(agent, None);
}

fn claim_comment_count(mcp: &AgentflareMcp, item_id: &str) -> usize {
    mcp.with_backend_db(|conn| agentflare_backend::comment::list_by_item(conn, item_id).unwrap())
        .unwrap()
        .iter()
        .filter(|c| c.body.starts_with(CI_SELF_REPAIR_MARKER))
        .count()
}

#[test]
fn unassigned_item_dispatches_repair_when_one_agent_resolves() {
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item_with_claim_age(&mcp, None, 1_900);
        let mut item = current_item(&mcp, &item_id);
        item.assignee_agent = None;
        // Models: no prior job, empty router, exactly one installed agent.
        TEST_FALLBACK_AGENT.with(|c| c.set(Some(Some(agent_registry::Agent::Opencode))));
        let outcome = self_repair_or_gate(
            &mcp,
            &queue,
            &test_auth_conn(),
            agentflare_resource_gate::Policy::Normal,
            &item,
            Some(&repair_pr(&item, 1)),
            RepairTrigger::FailingChecks(&["clippy".to_string()]),
            &[],
            &seed_gate_label(&mcp),
            "/repo",
        );
        TEST_FALLBACK_AGENT.with(|c| c.set(None));
        assert!(matches!(outcome, SelfRepairOutcome::Dispatched));
        assert_eq!(claim_comment_count(&mcp, &item_id), 1);
        let jobs = queue.list(None).unwrap();
        assert!(jobs.iter().any(|j| j.command == "agentflare-work"
            && j.args.first() == Some(&item_id)
            && j.args.get(1).map(String::as_str) == Some("opencode")));
    });
}

#[test]
fn unresolvable_agent_skips_without_a_claim_comment_or_job() {
    crate::paths::test_support::with_temp_home(|| {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_in_review_item_with_claim_age(&mcp, None, 1_900);
        let mut item = current_item(&mcp, &item_id);
        item.assignee_agent = None;
        TEST_FALLBACK_AGENT.with(|c| c.set(Some(None)));
        let label_id_by_name = seed_gate_label(&mcp);
        let auth_conn = test_auth_conn();
        let outcome = self_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            Some(&repair_pr(&item, 1)),
            RepairTrigger::FailingChecks(&["clippy".to_string()]),
            &[],
            &label_id_by_name,
            "/repo",
        );
        let cr_outcome = crate::supervisor::review_bots::coderabbit_repair_or_gate(
            &mcp,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            1,
            &[coderabbit_finding(1, "coderabbitai[bot]")],
            &[],
            &label_id_by_name,
            "/repo",
            None,
        );
        TEST_FALLBACK_AGENT.with(|c| c.set(None));
        assert!(matches!(outcome, SelfRepairOutcome::Skipped));
        assert!(matches!(cr_outcome, SelfRepairOutcome::Skipped));
        assert_eq!(claim_comment_count(&mcp, &item_id), 0);
        assert!(queue.list(None).unwrap().is_empty());
    });
}

#[test]
fn no_repair_agent_message_names_the_pr_and_item() {
    let msg = no_repair_agent_message(931, "item-248");
    assert!(
        msg.starts_with("agentflare-supervisor: no agent for repair of PR #931 (item item-248): "),
        "{msg}"
    );
}
