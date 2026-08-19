use super::*;

#[test]
fn work_item_data_round_trips_through_json() {
    let data = WorkItemData {
        reply_text: "did the thing".into(),
        session_id: Some("sess-1".into()),
        cost_usd: Some(0.42),
        hold_reason: None,
        review_issues: Some("- fix the thing".into()),
        pr_url: Some("https://github.com/x/y/pull/1".into()),
        ..Default::default()
    };
    let json = serde_json::to_string(&data).unwrap();
    let back: WorkItemData = serde_json::from_str(&json).unwrap();
    assert_eq!(back.reply_text, "did the thing");
    assert_eq!(
        back.pr_url.as_deref(),
        Some("https://github.com/x/y/pull/1")
    );
}

// Item #489: judge's raw Claude Code reply is a multi-line stream-json
// transcript; parsing its first line (action-less) instead of the last
// line's decision caused "missing field `action`" on #478/#502/#503.
const JUDGE_STREAM_JSON_TRANSCRIPT: &str = concat!(
    r#"{"type":"system","subtype":"init","cwd":"/work/.worktrees/task/478","session_id":"sess-1"}"#,
    "\n",
    r#"{"type":"result","result":"{\"action\":\"advance_task\",\"rationale\":\"done\",\"ledger_line\":\"Task 0: complete\",\"task_model_tier\":null}","session_id":"sess-1"}"#,
);

#[test]
fn uncleaned_claude_stream_json_transcript_breaks_judge_parsing() {
    let err = parse_judge_decision(JUDGE_STREAM_JSON_TRANSCRIPT).unwrap_err();
    assert!(
        err.to_string().contains("missing field `action`"),
        "expected the exact upstream failure signature, got: {err}"
    );
}

#[test]
fn clean_agent_reply_fixes_claude_stream_json_so_judge_parsing_succeeds() {
    let cleaned = crate::agent_launch::clean_agent_reply(
        agent_registry::Agent::ClaudeCode.as_str(),
        JUDGE_STREAM_JSON_TRANSCRIPT.to_string(),
    );
    let decision = parse_judge_decision(&cleaned).expect("should parse after cleaning");
    assert_eq!(decision.action, JudgeAction::AdvanceTask);
}

use flare_workflow::store::InMemoryStore;
use flare_workflow::{WorkflowDefinition, WorkflowEngine, WorkflowId};
use std::sync::Arc;

#[tokio::test]
#[ignore = "item_done hard-fails (#482) unless push succeeds AND a PR is \
            created; push_and_open_pr can't recognize a local bare repo \
            as GitHub, and this codebase has no mock GitHub client — \
            needs a real GitHub remote + credentials to reach Completed"]
async fn finalize_step_calls_item_done_on_success() {
    let (mcp, _backend_tmp, _repo_tmp, item_id, project_id, worktree_path) =
        crate::mcp_server::tests::mcp_with_claimed_item("Finalize test item");
    // Something real to commit — otherwise `item_done` sees a
    // never-diverged branch and treats it as a no-op ("unchanged")
    // rather than a completion.
    std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();
    let mcp = Arc::new(mcp);

    let data = WorkItemData {
        reply_text: "implemented the thing".into(),
        ..Default::default()
    };
    let step = build_finalize_step(
        mcp.clone(),
        item_id.clone(),
        None,
        crate::claims::owner_id(),
    );
    let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
    let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new(WORKFLOW_ID), data, String::new())
        .await
        .unwrap();

    for _ in 0..50 {
        let state = engine.get_status(run_id).await.unwrap();
        if state.status == flare_workflow::WorkflowStatus::Completed {
            let completed_state_id = mcp
                .with_backend_db(|conn| {
                    agentflare_backend::state::list_by_project(conn, &project_id)
                        .unwrap()
                        .into_iter()
                        .find(|st| st.group_name == "completed")
                        .unwrap()
                        .id
                })
                .unwrap();
            let item = mcp
                .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
                .unwrap()
                .unwrap();
            assert_eq!(
                item.state_id, completed_state_id,
                "finalize must move the item to the project's real 'completed' state"
            );
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("finalize step did not complete");
}

/// Item #507: a review-only run must never reach `item_done`/PR flow —
/// proven here by the step succeeding at all. `item_done` hard-fails
/// without a real GitHub remote + push (see the `#[ignore]`d
/// `finalize_step_calls_item_done_on_success` above), so if this branch
/// fell through to that path instead of the review_only short-circuit,
/// this test would fail the same way. Also asserts the findings landed
/// as a comment, which is the actual deliverable for a review task.
#[tokio::test]
async fn finalize_step_posts_review_findings_comment_and_skips_item_done_when_review_only() {
    let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, _worktree_path) =
        crate::mcp_server::tests::mcp_with_claimed_item("Review-only finalize test item");
    let mcp = Arc::new(mcp);

    let data = WorkItemData {
        review_only: true,
        last_report: Some("Found a SQL injection in the query builder.".to_string()),
        ..Default::default()
    };
    let step = build_finalize_step(
        mcp.clone(),
        item_id.clone(),
        None,
        crate::claims::owner_id(),
    );
    let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
    let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new(WORKFLOW_ID), data, String::new())
        .await
        .unwrap();

    for _ in 0..50 {
        let state = engine.get_status(run_id).await.unwrap();
        match state.status {
            flare_workflow::WorkflowStatus::Completed => {
                let comments: serde_json::Value = serde_json::from_str(
                    &mcp.comment_impl(CommentRequest {
                        action: "list".into(),
                        item_id: Some(item_id.clone()),
                        ..Default::default()
                    })
                    .unwrap(),
                )
                .unwrap();
                let arr = comments.as_array().unwrap();
                assert_eq!(
                    arr.len(),
                    1,
                    "review-only finalize must post exactly one findings comment"
                );
                let body = arr[0]["body"].as_str().unwrap();
                assert!(body.contains("review findings"));
                assert!(body.contains("SQL injection in the query builder"));
                return;
            }
            flare_workflow::WorkflowStatus::Failed => {
                panic!(
                    "finalize must not fail for a review-only task — it must not attempt \
                     item_done/PR flow: {:?}",
                    state.error
                );
            }
            _ => {}
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("finalize step did not complete");
}

/// Regression for the CodeRabbit finding on PR #547: a single-task
/// review-only run clears `last_report`/`review_issues` in the same
/// iteration it completes (see the `AdvanceTask`/`SkipTask` arm in
/// `sdd_loop`'s judge-decision handler), so by the time `finalize` runs
/// both are `None` even though the analyst produced real findings.
/// `finalize` must read the accumulated `review_findings` instead of
/// falling back to "No findings reported." in that case.
#[tokio::test]
async fn finalize_step_uses_accumulated_review_findings_when_last_report_was_cleared() {
    let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, _worktree_path) =
        crate::mcp_server::tests::mcp_with_claimed_item(
            "Review-only finalize accumulation test item",
        );
    let mcp = Arc::new(mcp);

    let data = WorkItemData {
        review_only: true,
        // Simulates post-AdvanceTask state: both cleared by the judge
        // decision handler, exactly as happens for a single-task run.
        last_report: None,
        review_issues: None,
        review_findings: vec!["Found a SQL injection in the query builder.".to_string()],
        ..Default::default()
    };
    let step = build_finalize_step(
        mcp.clone(),
        item_id.clone(),
        None,
        crate::claims::owner_id(),
    );
    let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
    let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new(WORKFLOW_ID), data, String::new())
        .await
        .unwrap();

    for _ in 0..50 {
        let state = engine.get_status(run_id).await.unwrap();
        match state.status {
            flare_workflow::WorkflowStatus::Completed => {
                let comments: serde_json::Value = serde_json::from_str(
                    &mcp.comment_impl(CommentRequest {
                        action: "list".into(),
                        item_id: Some(item_id.clone()),
                        ..Default::default()
                    })
                    .unwrap(),
                )
                .unwrap();
                let arr = comments.as_array().unwrap();
                let body = arr[0]["body"].as_str().unwrap();
                assert!(
                    body.contains("SQL injection in the query builder"),
                    "finalize must use review_findings, not fall back to \"No findings reported.\": {body}"
                );
                assert!(!body.contains("No findings reported."));
                return;
            }
            flare_workflow::WorkflowStatus::Failed => {
                panic!(
                    "finalize must not fail for a review-only task — it must not attempt \
                     item_done/PR flow: {:?}",
                    state.error
                );
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
        }
    }
    panic!("finalize step did not complete");
}

#[tokio::test]
async fn finalize_step_releases_claim_when_human_review_gate_is_hit() {
    let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, _worktree_path) =
        crate::mcp_server::tests::mcp_with_claimed_item("Human-review finalize test item");
    let mcp = Arc::new(mcp);
    let owner = crate::claims::owner_id();

    let data = WorkItemData {
        review_issues: Some("- still broken".into()),
        ..Default::default()
    };
    let step = build_finalize_step(mcp.clone(), item_id.clone(), None, owner.clone());
    let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
    let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new(WORKFLOW_ID), data, String::new())
        .await
        .unwrap();

    for _ in 0..50 {
        let state = engine.get_status(run_id).await.unwrap();
        if state.status == flare_workflow::WorkflowStatus::Completed {
            let still_claimed = mcp
                .with_backend_db(|conn| {
                    agentflare_backend::claim::is_owner(conn, &item_id, &owner)
                        .map_err(|e| e.to_string())
                })
                .unwrap()
                .unwrap();
            assert!(
                !still_claimed,
                "finalize must release the claim when gating for human review"
            );
            return;
        }
        if state.status == flare_workflow::WorkflowStatus::Failed {
            panic!(
                "finalize must succeed for human-review gate: {:?}",
                state.error
            );
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("finalize step did not complete");
}

#[tokio::test]
async fn finalize_step_releases_claim_after_review_only_success() {
    let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, _worktree_path) =
        crate::mcp_server::tests::mcp_with_claimed_item("Review-only claim release test item");
    let mcp = Arc::new(mcp);
    let owner = crate::claims::owner_id();

    let data = WorkItemData {
        review_only: true,
        last_report: Some("Found an issue.".to_string()),
        ..Default::default()
    };
    let step = build_finalize_step(mcp.clone(), item_id.clone(), None, owner.clone());
    let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
    let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new(WORKFLOW_ID), data, String::new())
        .await
        .unwrap();

    for _ in 0..50 {
        let state = engine.get_status(run_id).await.unwrap();
        if state.status == flare_workflow::WorkflowStatus::Completed {
            let still_claimed = mcp
                .with_backend_db(|conn| {
                    agentflare_backend::claim::is_owner(conn, &item_id, &owner)
                        .map_err(|e| e.to_string())
                })
                .unwrap()
                .unwrap();
            assert!(
                !still_claimed,
                "finalize must release the claim after a review-only run"
            );
            return;
        }
        if state.status == flare_workflow::WorkflowStatus::Failed {
            panic!("finalize must not fail for review-only: {:?}", state.error);
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("finalize step did not complete");
}

// requires a real headless agent binary; run manually / in an
// environment with one installed — the mock-sender variant right below
// covers the same metadata-persistence assertion unconditionally.
//
// Deliberately a plain `#[test]` (not `#[tokio::test]`): `run_or_resume`
// blocks synchronously via `crate::workflow::blocking_runtime().block_on`
// on a *separate* runtime (`WORKFLOW_RT`); calling it from inside an
// already-running tokio runtime (as an async test's own executor would
// be) panics with "Cannot start a runtime from within a runtime" — the
// same reason `src/workflow.rs`'s own `run_workflow_json`-driving tests
// are plain `#[test]`s too.
#[test]
#[ignore]
fn run_or_resume_persists_run_id_and_resume_skips_completed_coder_step() {
    let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, worktree_path) =
        crate::mcp_server::tests::mcp_with_claimed_item("Run-or-resume real-agent test item");
    std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();
    let mcp = Arc::new(mcp);
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
        .unwrap()
        .unwrap();

    // See the mock-sender variant below for why `engine()`'s db path
    // is isolated under a temp HOME.
    let result = crate::paths::test_support::with_temp_home(|| {
        run_or_resume(
            mcp.clone(),
            &item,
            agent_registry::Agent::ClaudeCode,
            agent_registry::Agent::ClaudeCode,
            "implement it".to_string(),
            None,
            None,
            std::time::Duration::from_secs(600),
            std::time::Duration::from_secs(300),
            Vec::new(),
        )
    });
    assert!(result.is_ok());

    let updated = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
        .unwrap()
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&updated.metadata).unwrap();
    assert!(metadata["workflow_run_id"].as_str().is_some());
}

/// Mock-sender counterpart of the real-agent test above: drives
/// `run_or_resume_with_sender` with a mock `SendMessage` that answers
/// `sdd_loop`'s implementer and judge roles (distinguished by prompt
/// content, same as `sdd_test_support::mock_send`'s callers), so it runs
/// unconditionally in CI. Unlike `finalize_step_calls_item_done_on_success`
/// this doesn't need a real GitHub PR to assert anything -- it only
/// checks that `workflow_run_id` was persisted before `finalize`'s
/// `item_done` call hard-fails on the missing PR (#482).
#[test]
fn run_or_resume_with_sender_persists_run_id_on_success() {
    let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, worktree_path) =
        crate::mcp_server::tests::mcp_with_claimed_item("Run-or-resume mock-sender test item");
    // Something real to commit — otherwise `finalize`'s `item_done` call
    // sees a never-diverged branch and treats it as a no-op rather than
    // a completion (see `finalize_step_calls_item_done_on_success`).
    std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();
    let mcp = Arc::new(mcp);
    let item = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
        .unwrap()
        .unwrap();

    let send: flare_workflow::json::SendMessage = Arc::new(
        move |inv: flare_workflow::json::StepInvocation| {
            let prompt = inv.prompt;
            Box::pin(async move {
                if prompt.contains("You are the judge") {
                    Ok((
                        r#"{"action":"complete_pipeline","rationale":"done","ledger_line":"Task 0: complete","task_model_tier":null}"#
                            .to_string(),
                        1u64,
                        0u64,
                    ))
                } else {
                    Ok(("DONE: did the work".to_string(), 1u64, 0u64))
                }
            })
        },
    );

    // `engine()` is a process-lifetime singleton keyed by
    // `crate::workflow::default_db_path()` (~/.agentflare/workflows.db)
    // — isolate it under a temp HOME for this call so the test neither
    // depends on nor pollutes the real user state dir (and works in
    // sandboxes where `$HOME` is read-only).
    let result = crate::paths::test_support::with_temp_home(|| {
        run_or_resume_with_sender(
            mcp.clone(),
            &item,
            agent_registry::Agent::ClaudeCode,
            agent_registry::Agent::ClaudeCode,
            "implement it".to_string(),
            None,
            None,
            send,
        )
    });
    // `mcp_with_claimed_item` wires a local bare `origin`, so `git push`
    // succeeds but not a real GitHub remote — `finalize`'s `item_done` call
    // hard-fails on the missing PR (item #109 / PR #482) -- same
    // reasoning as `finalize_step_calls_item_done_on_success` right
    // above, which needs `#[ignore]` for the same root cause since it
    // asserts completion rather than just metadata persistence. This
    // test only cares that `workflow_run_id` was persisted before that
    // failure, which happens well before `finalize` runs.
    assert!(result.is_err(), "{result:?}");

    let updated = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
        .unwrap()
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&updated.metadata).unwrap();
    assert!(metadata["workflow_run_id"].as_str().is_some());
}

// Item #512: bare `task/<N>` checkout + renamed slug hid divergence from
// `item_done`; finalize must surface workflow failure (not silent success).
#[tokio::test]
async fn finalize_step_fails_when_branch_slug_mismatch_hides_divergence() {
    let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, worktree_path) =
        crate::mcp_server::tests::mcp_with_claimed_item("!!!");
    mcp.with_backend_db(|conn| {
        agentflare_backend::item::update(
            conn,
            &item_id,
            agentflare_backend::item::UpdateItem {
                name: Some("Renamed Bugfix Item".into()),
                ..Default::default()
            },
        )
        .unwrap();
    })
    .unwrap();
    std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();
    let mcp = Arc::new(mcp);

    let data = WorkItemData {
        reply_text: "implemented the thing".into(),
        ..Default::default()
    };
    let step = build_finalize_step(
        mcp.clone(),
        item_id.clone(),
        None,
        crate::claims::owner_id(),
    );
    let wf = WorkflowDefinition::new(WORKFLOW_ID, "work item").add_step(step);
    let engine = WorkflowEngine::<WorkItemData, InMemoryStore<WorkItemData>>::new();
    engine.register_workflow(wf).unwrap();
    let run_id = engine
        .start_workflow(WorkflowId::new(WORKFLOW_ID), data, String::new())
        .await
        .unwrap();

    // `finalize` retries StepFailed up to 3× with 1s exponential backoff.
    for _ in 0..200 {
        let state = engine.get_status(run_id).await.unwrap();
        match state.status {
            flare_workflow::WorkflowStatus::Failed => {
                let err = state.error.unwrap_or_default();
                assert!(
                    err.contains("no PR resulted")
                        || err.contains("not marking completed")
                        || err.contains("One or more steps failed"),
                    "finalize must fail when item_done hard-errors: {err}"
                );
                let comments: serde_json::Value = serde_json::from_str(
                    &mcp.comment_impl(CommentRequest {
                        action: "list".into(),
                        item_id: Some(item_id.clone()),
                        ..Default::default()
                    })
                    .unwrap(),
                )
                .unwrap();
                let arr = comments.as_array().unwrap();
                assert!(
                    arr.iter().any(|c| c["body"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("PR creation failed")),
                    "finalize failure path must post PR-failure comment: {arr:?}"
                );
                return;
            }
            flare_workflow::WorkflowStatus::Completed => {
                panic!(
                    "finalize must not succeed when item_done errors on slug mismatch: {:?}",
                    state
                );
            }
            _ => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
    panic!("finalize step did not fail");
}

// Item #331's live failure: a double-JSON-encoded metadata field parses to
// `Value::String(...)`, not `Value::Object(...)` -- `Value`'s `IndexMut`
// panics assigning a key into anything that isn't already an object, so
// `persist_run_id` used to crash the whole job instead of just recording
// the run id against a freshly-coerced empty object.
#[test]
fn persist_run_id_recovers_from_non_object_existing_metadata() {
    let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, _worktree_path) =
        crate::mcp_server::tests::mcp_with_claimed_item("persist_run_id non-object metadata test");
    let mcp = Arc::new(mcp);

    let double_encoded = serde_json::Value::String(r#"{"size": "M"}"#.to_string());
    let run_id = flare_workflow::WorkflowRunId::new();

    let result = persist_run_id(&mcp, &item_id, &double_encoded, run_id);
    assert!(result.is_ok(), "{result:?}");

    let updated = mcp
        .with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id).ok())
        .unwrap()
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&updated.metadata).unwrap();
    assert_eq!(
        metadata["workflow_run_id"].as_str(),
        Some(run_id.to_string().as_str())
    );
}
