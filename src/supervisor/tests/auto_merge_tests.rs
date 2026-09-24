//! Auto-merge on CI-green + approval label (item #194): `merge_approved_pr`,
//! `merge_if_approved`, `merge_or_repair_findings`, and deleting a merged
//! PR's head branch (`delete_merged_head_branch_with`).
//!
//! Split into its own file to keep `supervisor_tests.rs` under the repo's
//! LOC gate (`scripts/loc-gate.sh`), mirroring `stray_pr_tests.rs`.

use super::*;

#[test]
fn merge_approved_pr_merges_via_squash_on_success() {
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(200, r#"{"merged":true}"#),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };

    assert!(merge_approved_pr(&client, &repo, 42, Some("abc123")));

    let reqs = server.requests();
    assert_eq!(reqs[0].method, "PUT");
    assert_eq!(reqs[0].path, "/repos/o/r/pulls/42/merge");
    let sent: serde_json::Value = serde_json::from_str(&reqs[0].body).unwrap();
    assert_eq!(sent["merge_method"], "squash");
    // Pinned to the head the sweep judged green.
    assert_eq!(sent["sha"], "abc123");
}

#[test]
fn merge_approved_pr_skips_when_the_head_moved_since_ci_was_checked() {
    // GitHub answers 409 when `sha` no longer matches the PR head: an agent
    // pushed after the sweep's snapshot. That is a plain skip, not a merge.
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(
            409,
            r#"{"message":"Head branch was modified. Review and try the merge again."}"#,
        ),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };

    assert!(!merge_approved_pr(&client, &repo, 42, Some("stale")));
    assert_eq!(server.requests().len(), 1, "no retry within the tick");
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
            r#"{"default_branch":"main","delete_branch_on_merge":false}"#,
        ),
        crate::github::test_support::MockResponse::json(200, r#"{"protected":false}"#),
        crate::github::test_support::MockResponse::json(204, ""),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };
    delete_merged_head_branch_with(&client, &repo, 42);
    let reqs = server.requests();
    assert_eq!(reqs.last().unwrap().method, "DELETE");
    assert_eq!(
        reqs.last().unwrap().path,
        "/repos/o/r/git/refs/heads/task/42"
    );
}

#[test]
fn merge_approved_pr_returns_false_and_does_not_panic_on_github_error() {
    // Branch protection / an unresolved conflict -- GitHub answers 405 on
    // the merge endpoint. The safety property is that this falls through to
    // `skipped` (no panic, no retry loop here); the sweep just polls again
    // next tick.
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(405, r#"{"message":"not mergeable"}"#),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };

    assert!(!merge_approved_pr(&client, &repo, 42, None));
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

    let merged = merge_if_approved(&mcp, &item, repo.path(), 42, &["size/s".to_string()], None);

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
        CiGreenMerge::Allowed { head_sha: None },
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
