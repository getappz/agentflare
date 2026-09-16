//! Item #234: an item can regress out of "in_review" (e.g. an orphaned
//! self-repair job's claim getting reconciled) while its PR is still open
//! and tracked, leaving it permanently invisible to this sweep's
//! state-group query. `stray_pr_is_still_relevant`'s own unit tests (below,
//! against a mock GitHub server) pin the actual open/merged/closed decision
//! that gates restoring such an item to "in_review" -- `run_review_sweep`'s
//! own integration tests here can only exercise the network-free half of
//! that decision, since `Client::new()` always targets real GitHub with
//! real credentials and can't be pointed at a mock server (mirrors every
//! other GitHub-touching branch in this sweep, e.g.
//! `run_review_sweep_never_merges_when_the_approval_label_only_exists_on_the_project_not_the_pr`'s
//! own doc comment on the same limitation).
//!
//! Split into its own file to keep `supervisor_tests.rs` under the repo's
//! LOC gate (`scripts/loc-gate.sh`), mirroring `host_gate_tests.rs`.

use super::*;

#[test]
fn run_review_sweep_leaves_a_stray_pr_item_alone_when_its_pr_state_cannot_be_verified() {
    // `throwaway_repo` has no git remote at all, so `RepoId::resolve_from_remote`
    // returns `None` before any network call -- there is no way to confirm
    // the tracked PR is still open, so the safe default is to leave the
    // item exactly where it is rather than blindly restoring it (the
    // correctness bug a first attempt at this fix had: restoring purely on
    // metadata presence + claim liveness, never checking GitHub's live PR
    // state).
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let queue = test_queue();
    let item_id = mcp
        .with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            let started =
                agentflare_backend::state::first_in_group(conn, &project.id, "started").unwrap();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id: started.id,
                    name: "Fix CI".into(),
                    description: Some("do it well".into()),
                    priority: None,
                    parent_id: None,
                    assignee_agent: Some("claude-code".into()),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: Some(r#"{"pr":{"number":501,"branch":"task/501"}}"#.into()),
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                    start_date: None,
                    due_date: None,
                },
            )
            .unwrap();
            item.id
        })
        .unwrap();

    let auth_conn = test_auth_conn();
    let result = run_review_sweep(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    // Never entered the batch at all -- no remote means the candidate was
    // never restored, so nothing here counted it as skipped either.
    assert_eq!(result.skipped, 0);
    let group = mcp
        .with_backend_db(|conn| {
            let item = agentflare_backend::item::get(conn, &item_id).unwrap();
            agentflare_backend::state::get(conn, &item.state_id)
                .unwrap()
                .group_name
        })
        .unwrap();
    assert_eq!(
        group, "started",
        "a stray item must not be restored to in_review without a live PR check confirming it"
    );
}

#[test]
fn run_review_sweep_leaves_a_stray_pr_item_alone_when_gated_by_needs_manual_dispatch() {
    // `NEEDS_MANUAL_LABEL` means the dispatch-failure cap already tripped
    // for this item and a human hasn't cleared it yet -- the self-heal must
    // never silently undo that gate just because the item still carries a
    // tracked PR number and has no live claim (review finding on item
    // #234's first attempt: `handle_terminal_job_failure` deliberately
    // leaves state group and `metadata.pr` untouched when it lands here).
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let queue = test_queue();
    let item_id = mcp
        .with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            let started =
                agentflare_backend::state::first_in_group(conn, &project.id, "started").unwrap();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id: started.id,
                    name: "Fix CI".into(),
                    description: Some("do it well".into()),
                    priority: None,
                    parent_id: None,
                    assignee_agent: Some("claude-code".into()),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: Some(r#"{"pr":{"number":501,"branch":"task/501"}}"#.into()),
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                    start_date: None,
                    due_date: None,
                },
            )
            .unwrap();
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
            .unwrap();
            let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
            let manual_id = &labels
                .iter()
                .find(|l| l.name == NEEDS_MANUAL_LABEL)
                .unwrap()
                .id;
            agentflare_backend::item::add_label(conn, &item.id, manual_id).unwrap();
            item.id
        })
        .unwrap();

    let auth_conn = test_auth_conn();
    let result = run_review_sweep(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.skipped, 0);
    let group = mcp
        .with_backend_db(|conn| {
            let item = agentflare_backend::item::get(conn, &item_id).unwrap();
            agentflare_backend::state::get(conn, &item.state_id)
                .unwrap()
                .group_name
        })
        .unwrap();
    assert_eq!(
        group, "started",
        "an item capped with needs-manual-dispatch must not be dragged back into in_review"
    );
}

#[test]
fn run_review_sweep_leaves_a_stray_pr_item_alone_when_gated_by_needs_decision() {
    // Same rationale as the `needs-manual-dispatch` case above, for the
    // go/no-go gate instead of the dispatch-failure cap.
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let queue = test_queue();
    let item_id = mcp
        .with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            let started =
                agentflare_backend::state::first_in_group(conn, &project.id, "started").unwrap();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id: started.id,
                    name: "Fix CI".into(),
                    description: Some("do it well".into()),
                    priority: None,
                    parent_id: None,
                    assignee_agent: Some("claude-code".into()),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: Some(r#"{"pr":{"number":501,"branch":"task/501"}}"#.into()),
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                    start_date: None,
                    due_date: None,
                },
            )
            .unwrap();
            agentflare_backend::label::create(
                conn,
                agentflare_backend::label::CreateLabel {
                    project_id: Some(project.id.clone()),
                    workspace_id: project.workspace_id.clone(),
                    name: NEEDS_DECISION_LABEL.into(),
                    color: None,
                    parent_id: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap();
            let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
            let gate_id = &labels
                .iter()
                .find(|l| l.name == NEEDS_DECISION_LABEL)
                .unwrap()
                .id;
            agentflare_backend::item::add_label(conn, &item.id, gate_id).unwrap();
            item.id
        })
        .unwrap();

    let auth_conn = test_auth_conn();
    let result = run_review_sweep(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.skipped, 0);
    let group = mcp
        .with_backend_db(|conn| {
            let item = agentflare_backend::item::get(conn, &item_id).unwrap();
            agentflare_backend::state::get(conn, &item.state_id)
                .unwrap()
                .group_name
        })
        .unwrap();
    assert_eq!(
        group, "started",
        "an item gated by needs-decision must not be dragged back into in_review"
    );
}

#[test]
fn stray_pr_is_still_relevant_true_for_an_open_pr() {
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(
            200,
            r#"{"number":501,"html_url":"u","state":"open","title":"t"}"#,
        ),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };

    assert!(stray_pr_is_still_relevant(&client, &repo, 501));
}

#[test]
fn stray_pr_is_still_relevant_true_for_a_merged_pr() {
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(
            200,
            r#"{"number":501,"html_url":"u","state":"closed","title":"t","merged_at":"2026-09-15T00:00:00Z"}"#,
        ),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };

    assert!(stray_pr_is_still_relevant(&client, &repo, 501));
}

#[test]
fn stray_pr_is_still_relevant_false_for_a_pr_closed_without_merging() {
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(
            200,
            r#"{"number":501,"html_url":"u","state":"closed","title":"t"}"#,
        ),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };

    assert!(!stray_pr_is_still_relevant(&client, &repo, 501));
}

#[test]
fn stray_pr_is_still_relevant_false_on_a_github_error() {
    let server = crate::github::test_support::MockServer::start(vec![
        crate::github::test_support::MockResponse::json(404, r#"{"message":"not found"}"#),
    ]);
    let client = server.client(Some("tok"));
    let repo = crate::github::RepoId {
        owner: "o".into(),
        repo: "r".into(),
    };

    assert!(!stray_pr_is_still_relevant(&client, &repo, 501));
}

#[test]
fn run_review_sweep_leaves_a_stray_pr_item_alone_while_its_claim_is_still_live() {
    let repo = throwaway_repo();
    let mcp = test_mcp_with_repo(repo.path().to_path_buf());
    let queue = test_queue();
    let item_id = mcp
        .with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            let started =
                agentflare_backend::state::first_in_group(conn, &project.id, "started").unwrap();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id: started.id,
                    name: "Fix CI".into(),
                    description: Some("do it well".into()),
                    priority: None,
                    parent_id: None,
                    assignee_agent: Some("claude-code".into()),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: Some(r#"{"pr":{"number":501,"branch":"task/501"}}"#.into()),
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                    start_date: None,
                    due_date: None,
                },
            )
            .unwrap();
            // A live claim, mirroring a self-repair job that's genuinely
            // still running -- `item::claim`'s doc comment is explicit that
            // this is what keeps such an item out of the sweep on purpose.
            agentflare_backend::claim::acquire(
                conn,
                &item.id,
                "claude-code:self-repair-job",
                crate::claims::now(),
                crate::claims::ttl_secs(),
            )
            .unwrap();
            item.id
        })
        .unwrap();

    let auth_conn = test_auth_conn();
    let result = run_review_sweep(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(
        result.skipped, 0,
        "a stray item still genuinely claimed must not be pulled into this sweep"
    );
    let group = mcp
        .with_backend_db(|conn| {
            let item = agentflare_backend::item::get(conn, &item_id).unwrap();
            agentflare_backend::state::get(conn, &item.state_id)
                .unwrap()
                .group_name
        })
        .unwrap();
    assert_eq!(
        group, "started",
        "an actively-claimed item's state must not be touched"
    );
}
