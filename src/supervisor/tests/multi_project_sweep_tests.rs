//! Multi-project autonomy for the review sweep and discovery tick: every
//! per-item action must land on items of projects other than the one the
//! daemon's own `mcp` is linked to, a PR closed without merging must send
//! its item back for a fresh attempt instead of wedging it in "in_review",
//! and one project's missing folder must not stall the others.
//!
//! Split into its own file to keep `supervisor_tests.rs` under the repo's
//! LOC gate (`scripts/loc-gate.sh`), mirroring `stray_pr_tests.rs`.

use super::*;

fn project_of(mcp: &AgentflareMcp, item_id: &str) -> String {
    mcp.with_backend_db(|conn| {
        agentflare_backend::item::get(conn, item_id)
            .unwrap()
            .project_id
    })
    .unwrap()
}

fn state_group_of(mcp: &AgentflareMcp, item_id: &str) -> String {
    mcp.with_backend_db(|conn| {
        let item = agentflare_backend::item::get(conn, item_id).unwrap();
        agentflare_backend::state::get(conn, &item.state_id)
            .unwrap()
            .group_name
    })
    .unwrap()
}

fn empty_sweep_result() -> ReviewSweepResult {
    ReviewSweepResult {
        promoted: 0,
        self_repaired: 0,
        review_repaired: 0,
        skipped: 0,
        waiting: 0,
        updated: 0,
        discovered: 0,
        requeued: 0,
    }
}

#[test]
fn a_project_scoped_view_resolves_items_the_daemons_own_project_cannot() {
    // The daemon's `mcp` is linked to its own project; every MCP entry
    // point the sweep uses (`item_check_merge`, `comment_impl`, ...)
    // resolves ids through `resolve_item_id`, which rejects any other
    // project's item. The sweep's per-batch scoped view must accept it.
    let repo_b = throwaway_repo();
    let mcp = test_mcp();
    let item_id = seed_in_review_item_in_project(&mcp, "proj-b", &repo_b.path().to_string_lossy());
    let comment = |m: &AgentflareMcp| {
        m.comment_impl(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.clone()),
            body: Some("hello".into()),
            ..Default::default()
        })
    };
    assert!(
        comment(&mcp).is_err(),
        "precondition: the unscoped daemon view rejects another project's item"
    );

    let scoped = mcp.scoped_to_project(project_of(&mcp, &item_id), repo_b.path().to_path_buf());
    comment(&scoped).expect("the scoped view must resolve the item");
    let merge: serde_json::Value = serde_json::from_str(
        &scoped
            .item_check_merge(ItemRequest {
                action: "check_merge".into(),
                id: Some(item_id.clone()),
                ..Default::default()
            })
            .expect("check_merge must resolve the item through the scoped view"),
    )
    .unwrap();
    assert_eq!(merge["promoted"], false, "no remote -> not merged yet");
}

#[test]
fn a_pr_closed_without_merging_requeues_its_item_exactly_once() {
    let repo_b = throwaway_repo();
    let mcp = test_mcp();
    let queue = test_queue();
    let auth_conn = test_auth_conn();
    let folder = repo_b.path().to_string_lossy().to_string();
    let item_id = seed_in_review_item_in_project(&mcp, "proj-b", &folder);
    let project_id = project_of(&mcp, &item_id);
    let ready_id = mcp
        .with_backend_db(|conn| {
            let project = agentflare_backend::project::get(conn, &project_id).unwrap();
            agentflare_backend::item::update(
                conn,
                &item_id,
                agentflare_backend::item::UpdateItem {
                    metadata: Some(r#"{"pr":{"number":42,"branch":"task/1"},"size":"S"}"#.into()),
                    ..Default::default()
                },
            )
            .unwrap();
            agentflare_backend::label::create(
                conn,
                agentflare_backend::label::CreateLabel {
                    project_id: Some(project_id.clone()),
                    workspace_id: project.workspace_id,
                    name: READY_LABEL.into(),
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
    let scoped = mcp.scoped_to_project(project_id, repo_b.path().to_path_buf());
    let item = scoped
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).unwrap())
        .unwrap();

    let mut result = empty_sweep_result();
    for _ in 0..2 {
        handle_pr_status(
            &scoped,
            &queue,
            &auth_conn,
            agentflare_resource_gate::Policy::Normal,
            &item,
            crate::worktree::PrCiStatus::Closed { number: 42 },
            &std::collections::HashMap::new(),
            &folder,
            repo_b.path(),
            &mut result,
        );
    }

    assert_eq!(result.requeued, 1, "requeued once, then a no-op");
    assert_eq!(state_group_of(&mcp, &item_id), "backlog");
    let (labels, metadata, owner, comments) = mcp
        .with_backend_db(|conn| {
            (
                agentflare_backend::item::list_labels(conn, &item_id).unwrap(),
                agentflare_backend::item::get(conn, &item_id)
                    .unwrap()
                    .metadata,
                agentflare_backend::claim::current_owner(conn, &item_id),
                agentflare_backend::comment::list_by_item(conn, &item_id).unwrap(),
            )
        })
        .unwrap();
    assert!(labels.contains(&ready_id), "re-armed with ready-for-work");
    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert!(metadata.get("pr").is_none(), "dead PR cleared: {metadata}");
    assert_eq!(metadata["size"], "S", "unrelated metadata kept");
    assert_eq!(owner, None, "the abandoned attempt's lease is released");
    assert_eq!(
        comments
            .iter()
            .filter(|c| c.body.contains("closed without merging"))
            .count(),
        1
    );
    assert!(queue.list(None).unwrap().is_empty(), "never self-repaired");
}

#[test]
fn job_in_flight_sees_an_active_job_older_than_the_newest_hundred() {
    let queue = test_queue();
    let job = |item: &str| {
        agentflare_jobs::AgentJob::new("agentflare-work")
            .args(vec![item.to_string(), "claude-code".into()])
            .in_process()
    };
    queue.enqueue(&job("old-item")).unwrap();
    for i in 0..105 {
        queue.enqueue(&job(&format!("other-{i}"))).unwrap();
    }

    assert!(job_in_flight(&queue, "old-item"));
    assert!(!job_in_flight(&queue, "never-queued"));
}

#[test]
fn run_discovery_tick_skips_a_project_whose_folder_is_gone_but_dispatches_the_rest() {
    let mcp = test_mcp();
    let queue = test_queue();
    let live = tempfile::tempdir().unwrap();
    let live_item = seed_ready_item_in_project(&mcp, "proj-live", &live.path().to_string_lossy());
    let gone_item = seed_ready_item_in_project(&mcp, "proj-gone", "/nonexistent/agentflare/repo");

    let auth_conn = test_auth_conn();
    let result = run_discovery_tick(
        &mcp,
        &queue,
        &auth_conn,
        agentflare_resource_gate::Policy::Normal,
    );

    assert_eq!(result.dispatched, 1);
    let jobs = queue.list(None).unwrap();
    assert!(jobs.iter().any(|j| j.args.contains(&live_item)));
    assert!(!jobs.iter().any(|j| j.args.contains(&gone_item)));
    // The per-project fairness count keys on the job's folder argument.
    let live_path = live.path().to_string_lossy().to_string();
    assert_eq!(queue.count_active_with_arg(&live_path, Some(2)).unwrap(), 1);
    assert_eq!(queue.count_active_with_arg(&live_path, Some(1)).unwrap(), 0);
}
