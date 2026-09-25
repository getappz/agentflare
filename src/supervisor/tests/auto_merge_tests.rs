//! Auto-merge on CI-green + approval label (item #194): `merge_approved_pr`,
//! `merge_if_approved`, `merge_or_repair_findings`, and deleting a merged
//! PR's head branch (`delete_merged_head_branch_with`).
//!
//! Split into its own file to keep `supervisor_tests.rs` under the repo's
//! LOC gate (`scripts/loc-gate.sh`), mirroring `stray_pr_tests.rs`.

use super::*;
use crate::github::test_support::{MockResponse, MockServer};
use crate::worktree::AutoMergeRef;

fn gh_repo() -> crate::github::RepoId {
    crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    }
}

/// `GET /repos/o/r`: the settings read `merge_approved_pr` makes first
/// (cached per mock server, so once per test).
fn repo_settings(allow_auto_merge: bool, allow_squash: bool) -> MockResponse {
    MockResponse::json(
        200,
        &format!(
            r#"{{"default_branch":"main","allow_auto_merge":{allow_auto_merge},"allow_squash_merge":{allow_squash}}}"#
        ),
    )
}

fn allowed<'a>(head_sha: Option<&'a str>, auto_merge: &'a AutoMergeRef) -> CiGreenMerge<'a> {
    CiGreenMerge::Allowed {
        head_sha,
        auto_merge,
    }
}

#[test]
fn merge_approved_pr_merges_directly_with_the_repos_method_when_auto_merge_is_off() {
    let server = MockServer::start(vec![
        repo_settings(false, true),
        MockResponse::json(200, r#"{"merged":true}"#),
    ]);
    let client = server.client(Some("tok"));
    let auto = AutoMergeRef {
        node_id: Some("PR_1".into()),
        enabled: false,
    };

    assert_eq!(
        merge_approved_pr(&client, &gh_repo(), 42, allowed(Some("abc123"), &auto)),
        MergeAttempt::Merged
    );

    let reqs = server.requests();
    assert_eq!(reqs[0].path, "/repos/o/r", "settings are read first");
    assert_eq!(reqs[1].method, "PUT");
    assert_eq!(reqs[1].path, "/repos/o/r/pulls/42/merge");
    let sent: serde_json::Value = serde_json::from_str(&reqs[1].body).unwrap();
    assert_eq!(sent["merge_method"], "squash");
    // Pinned to the head the sweep judged green.
    assert_eq!(sent["sha"], "abc123");
    assert_eq!(
        reqs.len(),
        2,
        "no GraphQL call when the repo has auto-merge off"
    );
}

#[test]
fn merge_approved_pr_uses_a_merge_commit_when_the_repo_forbids_squash() {
    let server = MockServer::start(vec![
        repo_settings(false, false),
        MockResponse::json(200, r#"{"merged":true}"#),
    ]);
    let client = server.client(Some("tok"));
    let auto = AutoMergeRef::default();
    assert_eq!(
        merge_approved_pr(&client, &gh_repo(), 42, allowed(None, &auto)),
        MergeAttempt::Merged
    );
    let reqs = server.requests();
    let sent: serde_json::Value = serde_json::from_str(&reqs[1].body).unwrap();
    assert_eq!(sent["merge_method"], "merge");
}

#[test]
fn merge_approved_pr_arms_github_auto_merge_pinned_to_the_head_when_the_repo_allows_it() {
    let server = MockServer::start(vec![
        repo_settings(true, true),
        MockResponse::json(
            200,
            r#"{"data":{"enablePullRequestAutoMerge":{"pullRequest":{"autoMergeRequest":{"enabledAt":"x"}}}}}"#,
        ),
        MockResponse::json(201, r#"{"id":1}"#),
    ]);
    let client = server.client(Some("tok"));
    let auto = AutoMergeRef {
        node_id: Some("PR_1".into()),
        enabled: false,
    };

    assert_eq!(
        merge_approved_pr(&client, &gh_repo(), 42, allowed(Some("abc123"), &auto)),
        MergeAttempt::AutoMergeArmed
    );

    let reqs = server.requests();
    assert_eq!(
        reqs.len(),
        3,
        "armed and stamped: no direct merge call follows"
    );
    assert_eq!(reqs[1].path, "/graphql");
    let sent: serde_json::Value = serde_json::from_str(&reqs[1].body).unwrap();
    assert_eq!(sent["variables"]["input"]["pullRequestId"], "PR_1");
    assert_eq!(sent["variables"]["input"]["mergeMethod"], "SQUASH");
    assert_eq!(sent["variables"]["input"]["expectedHeadOid"], "abc123");
    // The judged head is stamped so a repo requiring the context lets
    // GitHub merge exactly this head, not a later push.
    assert_eq!(reqs[2].method, "POST");
    assert_eq!(reqs[2].path, "/repos/o/r/statuses/abc123");
    let status: serde_json::Value = serde_json::from_str(&reqs[2].body).unwrap();
    assert_eq!(status["state"], "success");
    assert_eq!(status["context"], JUDGED_STATUS_CONTEXT);
}

#[test]
fn merge_approved_pr_arms_auto_merge_on_a_review_blocked_pr_but_never_merges_it_directly() {
    let server = MockServer::start(vec![
        repo_settings(true, true),
        MockResponse::json(
            200,
            r#"{"data":{"enablePullRequestAutoMerge":{"pullRequest":{"autoMergeRequest":{"enabledAt":"x"}}}}}"#,
        ),
    ]);
    let client = server.client(Some("tok"));
    let auto = AutoMergeRef {
        node_id: Some("PR_1".into()),
        enabled: false,
    };
    let blocked = CiGreenMerge::BlockedOnReview {
        changes_requested: false,
        head_sha: Some("abc123"),
        auto_merge: &auto,
    };
    assert_eq!(
        merge_approved_pr(&client, &gh_repo(), 42, blocked),
        MergeAttempt::AutoMergeArmed
    );
    // Settings, the arming mutation, and the judged-head status; a failed
    // status post (no canned response left) is soft, never a merge attempt.
    assert_eq!(server.requests().len(), 3);

    // Arming refused (or the repo has auto-merge off): a review-blocked PR
    // gets no direct merge attempt either -- GitHub would only refuse it.
    let server = MockServer::start(vec![repo_settings(false, true)]);
    let client = server.client(Some("tok"));
    assert_eq!(
        merge_approved_pr(&client, &gh_repo(), 42, blocked),
        MergeAttempt::NotMerged
    );
    assert_eq!(server.requests().len(), 1, "settings only");
}

#[test]
fn merge_approved_pr_falls_back_to_a_direct_merge_when_arming_auto_merge_is_refused() {
    // GitHub refuses to arm auto-merge on a PR it could merge right now
    // ("clean status"); the direct merge is the fallback, not a skip.
    let server = MockServer::start(vec![
        repo_settings(true, true),
        MockResponse::json(
            200,
            r#"{"data":{"enablePullRequestAutoMerge":null},"errors":[{"message":"Pull request is in clean status"}]}"#,
        ),
        MockResponse::json(200, r#"{"merged":true}"#),
    ]);
    let client = server.client(Some("tok"));
    let auto = AutoMergeRef {
        node_id: Some("PR_1".into()),
        enabled: false,
    };
    assert_eq!(
        merge_approved_pr(&client, &gh_repo(), 42, allowed(Some("abc123"), &auto)),
        MergeAttempt::Merged
    );
    let reqs = server.requests();
    assert_eq!(reqs[2].path, "/repos/o/r/pulls/42/merge");
}

#[test]
fn merge_approved_pr_does_not_rearm_an_already_armed_auto_merge() {
    let server = MockServer::start(vec![repo_settings(true, true)]);
    let client = server.client(Some("tok"));
    let auto = AutoMergeRef {
        node_id: Some("PR_1".into()),
        enabled: true,
    };
    assert_eq!(
        merge_approved_pr(&client, &gh_repo(), 42, allowed(Some("abc123"), &auto)),
        MergeAttempt::AutoMergeArmed
    );
    assert_eq!(
        server.requests().len(),
        1,
        "settings only: GitHub is already on it"
    );
}

#[test]
fn merge_approved_pr_skips_when_the_head_moved_since_ci_was_checked() {
    // GitHub answers 409 when `sha` no longer matches the PR head: an agent
    // pushed after the sweep's snapshot. That is a plain skip, not a merge.
    let server = MockServer::start(vec![
        repo_settings(false, true),
        MockResponse::json(
            409,
            r#"{"message":"Head branch was modified. Review and try the merge again."}"#,
        ),
    ]);
    let client = server.client(Some("tok"));
    let auto = AutoMergeRef::default();

    assert_eq!(
        merge_approved_pr(&client, &gh_repo(), 42, allowed(Some("stale"), &auto)),
        MergeAttempt::NotMerged
    );
    assert_eq!(server.requests().len(), 2, "no retry within the tick");
}

#[test]
fn disarm_auto_merge_with_sends_the_disable_mutation_and_soft_fails() {
    let server = MockServer::start(vec![
        MockResponse::json(
            200,
            r#"{"data":{"disablePullRequestAutoMerge":{"pullRequest":{"number":42}}}}"#,
        ),
        MockResponse::json(500, r#"{"message":"boom"}"#),
    ]);
    let client = server.client(Some("tok"));
    assert!(disarm_auto_merge_with(
        &client,
        &gh_repo(),
        42,
        "PR_1",
        "test"
    ));
    // A failure only logs and reports false, so the caller keeps its
    // record and retries next tick; the sweep must not panic over it.
    assert!(!disarm_auto_merge_with(
        &client,
        &gh_repo(),
        42,
        "PR_1",
        "test"
    ));
    let reqs = server.requests();
    assert_eq!(reqs.len(), 2);
    let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
    assert!(
        sent["query"]
            .as_str()
            .unwrap()
            .contains("disablePullRequestAutoMerge")
    );
}

fn batch_snapshot(
    head: &str,
    auto_merge_enabled: bool,
    merged: bool,
) -> crate::github::graphql::BatchPrData {
    crate::github::graphql::BatchPrData {
        merged,
        closed: false,
        mergeable: Some(true),
        mergeable_state: Some("clean".into()),
        checks: vec![],
        labels: vec![],
        head_sha: Some(head.to_string()),
        review_decision: None,
        rollup_state: None,
        node_id: Some("PR_1".into()),
        auto_merge_enabled,
        merge_queue_enabled: false,
        in_merge_queue: false,
        is_draft: false,
    }
}

#[test]
fn judge_armed_auto_merge_disarms_on_a_moved_head_and_forgets_a_gone_arming() {
    let armed = ArmedAutoMerge {
        head: "abc".into(),
        node_id: "PR_1".into(),
    };
    // Same head, still armed: leave it.
    assert_eq!(
        judge_armed_auto_merge(&armed, Some(&batch_snapshot("abc", true, false))),
        ArmedVerdict::Keep
    );
    // A push moved the head: the new commits were never judged.
    assert_eq!(
        judge_armed_auto_merge(&armed, Some(&batch_snapshot("def", true, false))),
        ArmedVerdict::Disarm
    );
    // GitHub no longer has it armed (someone disarmed it, or it merged).
    assert_eq!(
        judge_armed_auto_merge(&armed, Some(&batch_snapshot("def", false, false))),
        ArmedVerdict::Forget
    );
    assert_eq!(
        judge_armed_auto_merge(&armed, Some(&batch_snapshot("abc", true, true))),
        ArmedVerdict::Forget
    );
    // No snapshot this tick: judge next tick instead.
    assert_eq!(judge_armed_auto_merge(&armed, None), ArmedVerdict::Keep);
    // Armed without a known head: a move can't be judged, so it is kept.
    let headless = ArmedAutoMerge {
        head: String::new(),
        node_id: "PR_1".into(),
    };
    assert_eq!(
        judge_armed_auto_merge(&headless, Some(&batch_snapshot("def", true, false))),
        ArmedVerdict::Keep
    );
}

#[test]
fn armed_auto_merge_is_recorded_in_pr_metadata_and_cleared_again() {
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let fetch = || {
        mcp.with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
            .unwrap()
    };
    // Existing `pr` fields survive the merge.
    mcp.with_backend_db(|conn| {
        crate::mcp_server::merge_item_metadata(conn, &item_id, |m| {
            m.insert(
                "pr".into(),
                serde_json::json!({"number": 42, "branch": "task/1"}),
            );
        })
        .unwrap()
    })
    .unwrap();
    let item = fetch();
    assert_eq!(armed_auto_merge(&item), None, "nothing recorded yet");
    // Nothing recorded: the label-absent path must not touch the network
    // (this repo has no remote, so a call would have nothing to reach
    // either way -- the assertion is on the return value).
    assert!(!disarm_our_auto_merge(&mcp, &item, repo.path(), 42, "test"));

    record_armed_auto_merge(&mcp, &item, Some("abc"), "PR_1");
    let item = fetch();
    assert_eq!(
        armed_auto_merge(&item),
        Some(ArmedAutoMerge {
            head: "abc".into(),
            node_id: "PR_1".into(),
        })
    );
    assert_eq!(crate::worktree::pr_number_from_metadata(&item), Some(42));

    // A gone arming is forgotten without any GitHub call.
    reconcile_armed_auto_merge(
        &mcp,
        &item,
        repo.path(),
        42,
        Some(&batch_snapshot("abc", false, false)),
    );
    assert_eq!(armed_auto_merge(&fetch()), None);
    assert_eq!(crate::worktree::pr_number_from_metadata(&fetch()), Some(42));
}

#[test]
fn delete_merged_head_branch_with_soft_fails_on_github_errors() {
    // A leftover branch must never turn into a panic after a promotion that
    // already happened.
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(500, r#"{"message":"boom"}"#),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };
    delete_merged_head_branch_with(&client, &repo, 42);
    assert_eq!(server.requests()[0].path, "/repos/o/r/pulls/42");
}

#[test]
fn delete_merged_head_branch_with_deletes_the_merged_head_ref() {
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(
            200,
            r#"{"number":42,"html_url":"u","state":"closed","title":"t","merged_at":"2026-09-20T00:00:00Z","head":{"ref":"task/42","sha":"abc","repo":{"full_name":"o/r"}}}"#,
        ),
        crate::github::test_support::MockResponse::json(
            200,
            r#"{"default_branch":"main","delete_branch_on_merge":false,"node_id":"R_node"}"#,
        ),
        crate::github::test_support::MockResponse::json(
            200,
            r#"{"protected":false,"commit":{"sha":"abc"}}"#,
        ),
        crate::github::test_support::MockResponse::json(
            200,
            r#"{"data":{"updateRefs":{"clientMutationId":null}}}"#,
        ),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };
    delete_merged_head_branch_with(&client, &repo, 42);
    // Deleted atomically via GraphQL `updateRefs` guarded on the merged
    // head sha -- never the unguarded REST ref DELETE.
    let reqs = server.requests();
    let last = reqs.last().unwrap();
    assert_eq!(last.method, "POST");
    assert_eq!(last.path, "/graphql");
    let sent: serde_json::Value = serde_json::from_str(&last.body).unwrap();
    let update = &sent["variables"]["input"]["refUpdates"][0];
    assert_eq!(update["name"], "refs/heads/task/42");
    assert_eq!(update["beforeOid"], "abc");
    assert!(reqs.iter().all(|r| r.method != "DELETE"));
}

#[test]
fn merge_approved_pr_returns_false_and_does_not_panic_on_github_error() {
    // Branch protection / an unresolved conflict -- GitHub answers 405 on
    // the merge endpoint. The safety property is that this falls through to
    // `skipped` (no panic, no retry loop here); the sweep just polls again
    // next tick.
    let server = MockServer::start(vec![
        repo_settings(false, true),
        MockResponse::json(405, r#"{"message":"not mergeable"}"#),
    ]);
    let client = server.client(Some("tok"));
    let auto = AutoMergeRef::default();

    assert_eq!(
        merge_approved_pr(&client, &gh_repo(), 42, allowed(None, &auto)),
        MergeAttempt::NotMerged
    );
}

#[test]
fn merge_if_approved_skips_without_touching_network_when_label_is_absent() {
    // No approval label on the PR -- CI green alone must never be enough to
    // merge. The label check must happen before any GitHub call, so this
    // must return false even with an unresolvable repo/no credentials.
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();

    let auto = AutoMergeRef::default();
    let merged = merge_if_approved(
        &mcp,
        &item,
        repo.path(),
        42,
        &["size/s".to_string()],
        allowed(None, &auto),
    );

    assert!(!merged);
    let still_in_review = mcp
        .with_backend_db(|conn| {
            let refetched = agentflare_backend::item::get(conn, &item_id).unwrap();
            let state = agentflare_backend::state::get(conn, &refetched.state_id).unwrap();
            state.group_name == "in_review"
        })
        .unwrap();
    assert!(still_in_review, "an unapproved item must not be promoted");
}

#[test]
fn run_review_sweep_never_merges_when_the_approval_label_only_exists_on_the_project_not_the_pr() {
    // Regression for the safety property in item #194's spec: the approval
    // label must gate on the PR's OWN GitHub labels (carried by
    // `PrCiStatus::Passing`), never merely on the label existing somewhere
    // in the project's label table. A throwaway repo with no remote always
    // resolves to `PrCiStatus::Unknown`, so this also covers Pending/Failing
    // by construction -- none of those variants carry PR labels for
    // `merge_if_approved` to check in the first place.
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let _item_id = seed_in_review_item(&mcp, Some("claude-code"));
    mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).unwrap();
        agentflare_backend::label::create(
            conn,
            agentflare_backend::label::CreateLabel {
                project_id: Some(project.id.clone()),
                workspace_id: project.workspace_id.clone(),
                name: PR_APPROVAL_LABEL.into(),
                color: None,
                parent_id: None,
                sort_order: None,
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
    })
    .unwrap();
    let queue = test_queue();
    let auth_conn = test_auth_conn();

    let result = run_review_sweep(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.promoted, 0);
    assert_eq!(result.skipped, 1);
}

#[test]
fn merge_or_repair_findings_never_attempts_a_merge_while_github_awaits_review() {
    // `AwaitingReview`: branch protection blocks the merge until a human
    // review lands, so even with the approval label and no findings the
    // merge must not be attempted -- this repo has no GitHub remote, so an
    // attempt would also have nothing to reach.
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let item_id = seed_in_review_item(&mcp, Some("claude-code"));
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let label_id_by_name = seed_gate_label(&mcp);
    let queue = test_queue();
    let auth_conn = test_auth_conn();

    let outcome = merge_or_repair_findings(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        repo.path(),
        42,
        &[],
        &[PR_APPROVAL_LABEL.to_string()],
        &label_id_by_name,
        "/repo",
        CiGreenMerge::BlockedOnReview {
            changes_requested: false,
            head_sha: None,
            auto_merge: &AutoMergeRef::default(),
        },
    );

    assert!(matches!(outcome, PassingPrOutcome::NotMerged));
}

#[test]
fn merge_or_repair_findings_never_merges_a_ci_green_approved_pr_with_unresolved_findings() {
    // Regression for item #628 (GitHub PR 791): an approved, CI-green PR
    // must not be merged while CodeRabbit findings are still unresolved on
    // it, no matter what `merge_if_approved` would otherwise decide. Claim
    // is backdated past the in_review TTL cap so the dispatch path is live
    // rather than gated, proving the findings genuinely routed to repair
    // instead of silently no-op'ing past both checks.
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let item_id = seed_in_review_item_with_claim_age(&mcp, Some("claude-code"), 1_900);
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();
    let label_id_by_name = seed_gate_label(&mcp);
    let queue = test_queue();
    let auth_conn = test_auth_conn();
    let findings = vec![coderabbit_finding(1, "coderabbitai[bot]")];

    let outcome = merge_or_repair_findings(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
        &item,
        repo.path(),
        42,
        &findings,
        &[PR_APPROVAL_LABEL.to_string()],
        &label_id_by_name,
        "/repo",
        allowed(None, &AutoMergeRef::default()),
    );

    assert!(
        !matches!(outcome, PassingPrOutcome::Merged),
        "unresolved CodeRabbit findings must block the merge even with the approval label present"
    );
    assert!(matches!(
        outcome,
        PassingPrOutcome::Repair(SelfRepairOutcome::Dispatched)
    ));
    let still_in_review = mcp
        .with_backend_db(|conn| {
            let refetched = agentflare_backend::item::get(conn, &item_id).unwrap();
            let state = agentflare_backend::state::get(conn, &refetched.state_id).unwrap();
            state.group_name == "in_review"
        })
        .unwrap();
    assert!(
        still_in_review,
        "an item with unresolved findings must not be promoted"
    );
}
