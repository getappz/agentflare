//! Regression tests for adopt-path repair dispatch (item #686 / #687).
use super::sdd_test_support::*;
use super::*;
use std::sync::Arc;

use flare_workflow::{StepResult, WorkflowId};

const REPAIR_MARKER: &str = "## supervisor — CodeRabbit review repair dispatched";
const UNIQUE_FINDING: &str = "UNIQUE_CODERABBIT_FINDING_687: null deref in parser.rs";

#[test]
fn sdd_loop_turn_uses_registered_owner_patch_at_spawn() {
    crate::paths::test_support::with_temp_home(|| {
        let hang_send: flare_workflow::json::SendMessage =
            Arc::new(|_inv| Box::pin(std::future::pending::<Result<(String, u64, u64), String>>()));
        let eng = engine();
        eng.register_workflow(
            flare_workflow::WorkflowDefinition::new(WORKFLOW_ID, "sdd work item")
                .add_step(build_sdd_loop_step(hang_send)),
        )
        .unwrap();
        let run_id = crate::workflow::blocking_runtime()
            .block_on(eng.start_workflow(
                WorkflowId::new(WORKFLOW_ID),
                WorkItemData {
                    agent_name: "implementer-agent".to_string(),
                    judge_agent_name: "judge-agent".to_string(),
                    owner: "claude-code:dead-job".into(),
                    tasks: one_task_data().tasks,
                    ..Default::default()
                },
                String::new(),
            ))
            .unwrap();
        crate::workflow::blocking_runtime()
            .block_on(eng.patch_run_data(run_id, |data: &mut WorkItemData| {
                data.owner = "claude-code:live-repair-job".into();
            }))
            .unwrap();

        let mut ctx = crate::workflow::blocking_runtime()
            .block_on(eng.get_status(run_id))
            .unwrap()
            .context;
        // Simulate an in-flight step that loaded `data` before the adopt rebind.
        ctx.data.owner = "claude-code:dead-job".into();
        ctx.run_id = run_id;

        let (send, _calls, owners) = mock_send_recording_owners(vec![
            "DONE: did the work",
            r#"{"action":"complete_pipeline","rationale":"done","ledger_line":"Task 0: complete","task_model_tier":null}"#,
        ]);
        let step = build_sdd_loop_step(send);
        let result = crate::workflow::blocking_runtime()
            .block_on(step.executor.execute(&mut ctx))
            .unwrap();
        assert!(matches!(result, StepResult::Success));

        let recorded = owners.lock().unwrap();
        assert_eq!(
            recorded.first().and_then(|o| o.as_deref()),
            Some("claude-code:live-repair-job"),
            "implementer turn must carry the adopting dispatch's claim owner"
        );
    });
}

#[test]
fn adopt_existing_run_injects_repair_findings_into_task_prompt() {
    crate::paths::test_support::with_temp_home(|| {
        let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, worktree) =
            crate::mcp_server::tests::mcp_with_claimed_item("Adopt repair dispatch");
        let mcp = Arc::new(mcp);
        let eng = engine();

        let hang_send: flare_workflow::json::SendMessage =
            Arc::new(|_inv| Box::pin(std::future::pending::<Result<(String, u64, u64), String>>()));
        eng.register_workflow(build_work_item_pipeline_with_sender(mcp.clone(), hang_send))
            .unwrap();
        let stale_owner = "claude-code:stale-implementer".to_string();
        let run_id = crate::workflow::blocking_runtime()
            .block_on(eng.start_workflow(
                WorkflowId::new(WORKFLOW_ID),
                WorkItemData {
                    item_id: item_id.clone(),
                    agent_name: agent_registry::Agent::ClaudeCode.as_str().to_string(),
                    judge_agent_name: agent_registry::Agent::ClaudeCode.as_str().to_string(),
                    owner: stale_owner.clone(),
                    worktree_path: worktree.display().to_string(),
                    tasks: vec![SddTask {
                        id: 0,
                        title: "Original item work".into(),
                        body: "Only the stale item description belongs here.".into(),
                        model_tier: None,
                    }],
                    ..Default::default()
                },
                String::new(),
            ))
            .unwrap();
        persist_run_id(&mcp, &item_id, run_id).unwrap();

        mcp.comment_impl(crate::mcp_server::types::CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.clone()),
            body: Some(format!(
                "{REPAIR_MARKER}\n\nThe review bot left 1 unresolved finding(s) on this PR:\n\n\
                 - parser.rs — possible null deref\n\n{UNIQUE_FINDING}\n\njob: repair-687"
            )),
            ..Default::default()
        })
        .unwrap();

        let (next_owner, _) = {
            let agent = crate::claims::agent_of(&crate::claims::owner_id()).to_string();
            let job_id = format!("adopt-repair-{}", std::process::id());
            (format!("{agent}:{job_id}"), job_id)
        };

        let incoming_tasks =
            load_or_synthesize_tasks("Only the stale item description belongs here.", None);
        let state = crate::workflow::blocking_runtime()
            .block_on(eng.get_status(run_id))
            .unwrap();
        crate::workflow::blocking_runtime()
            .block_on(adopt_existing_run(
                eng,
                &mcp,
                &item_id,
                run_id,
                &state,
                &next_owner,
                &incoming_tasks,
            ))
            .unwrap();

        let (send, calls, owners) = mock_send_recording_owners(vec![
            "DONE: fixed",
            r#"{"action":"complete_pipeline","rationale":"done","ledger_line":"Task 0: complete","task_model_tier":null}"#,
        ]);
        let step = build_sdd_loop_step(send);
        let mut ctx = crate::workflow::blocking_runtime()
            .block_on(eng.get_status(run_id))
            .unwrap()
            .context;
        ctx.run_id = run_id;
        crate::workflow::blocking_runtime()
            .block_on(step.executor.execute(&mut ctx))
            .unwrap();

        let recorded = calls.lock().unwrap();
        assert!(
            recorded[0].1.contains(UNIQUE_FINDING),
            "repair dispatch findings must be in the implementer prompt, got: {}",
            recorded[0].1
        );
        assert_eq!(
            owners.lock().unwrap().first().and_then(|o| o.as_deref()),
            Some(next_owner.as_str())
        );
    });
}

const UNIQUE_FINDING_2: &str = "UNIQUE_CODERABBIT_FINDING_687_B: second repair round";

#[test]
fn repair_dispatch_comment_not_re_adopted_without_new_marker_comment() {
    crate::paths::test_support::with_temp_home(|| {
        let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, worktree) =
            crate::mcp_server::tests::mcp_with_claimed_item("Adopt repair once");
        let mcp = Arc::new(mcp);
        let eng = engine();
        let hang_send: flare_workflow::json::SendMessage =
            Arc::new(|_inv| Box::pin(std::future::pending::<Result<(String, u64, u64), String>>()));
        eng.register_workflow(build_work_item_pipeline_with_sender(mcp.clone(), hang_send))
            .unwrap();
        let run_id = crate::workflow::blocking_runtime()
            .block_on(eng.start_workflow(
                WorkflowId::new(WORKFLOW_ID),
                WorkItemData {
                    item_id: item_id.clone(),
                    agent_name: agent_registry::Agent::ClaudeCode.as_str().to_string(),
                    judge_agent_name: agent_registry::Agent::ClaudeCode.as_str().to_string(),
                    owner: "claude-code:stale".into(),
                    worktree_path: worktree.display().to_string(),
                    tasks: vec![SddTask {
                        id: 0,
                        title: "Original".into(),
                        body: "work".into(),
                        model_tier: None,
                    }],
                    current_task_index: 0,
                    fix_round: 0,
                    ..Default::default()
                },
                String::new(),
            ))
            .unwrap();
        persist_run_id(&mcp, &item_id, run_id).unwrap();

        mcp.comment_impl(crate::mcp_server::types::CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.clone()),
            body: Some(format!("{REPAIR_MARKER}\n\n{UNIQUE_FINDING}\n")),
            ..Default::default()
        })
        .unwrap();

        let incoming = load_or_synthesize_tasks("work", None);
        let adopt = |owner: &str| {
            let state = crate::workflow::blocking_runtime()
                .block_on(eng.get_status(run_id))
                .unwrap();
            crate::workflow::blocking_runtime()
                .block_on(adopt_existing_run(
                    eng, &mcp, &item_id, run_id, &state, owner, &incoming,
                ))
                .unwrap();
        };
        adopt("claude-code:job-1");

        crate::workflow::blocking_runtime()
            .block_on(eng.state_store().update(run_id, |s| {
                s.context.data.current_task_index = 1;
                s.context.data.fix_round = 2;
            }))
            .unwrap();

        assert!(
            latest_repair_dispatch_body(&mcp, &item_id).is_none(),
            "consumed repair comment must not be offered again"
        );

        adopt("claude-code:job-2");

        let data = crate::workflow::blocking_runtime()
            .block_on(eng.get_status(run_id))
            .unwrap()
            .context
            .data;
        assert_eq!(
            data.current_task_index, 1,
            "second adopt must not reset index"
        );
        assert_eq!(data.fix_round, 2, "second adopt must not reset fix_round");
    });
}

#[test]
fn newer_repair_dispatch_comment_is_adopted_on_redispatch() {
    crate::paths::test_support::with_temp_home(|| {
        let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, worktree) =
            crate::mcp_server::tests::mcp_with_claimed_item("Adopt newer repair");
        let mcp = Arc::new(mcp);
        let eng = engine();
        let hang_send: flare_workflow::json::SendMessage =
            Arc::new(|_inv| Box::pin(std::future::pending::<Result<(String, u64, u64), String>>()));
        eng.register_workflow(build_work_item_pipeline_with_sender(mcp.clone(), hang_send))
            .unwrap();
        let run_id = crate::workflow::blocking_runtime()
            .block_on(eng.start_workflow(
                WorkflowId::new(WORKFLOW_ID),
                WorkItemData {
                    item_id: item_id.clone(),
                    agent_name: agent_registry::Agent::ClaudeCode.as_str().to_string(),
                    judge_agent_name: agent_registry::Agent::ClaudeCode.as_str().to_string(),
                    owner: "claude-code:stale".into(),
                    worktree_path: worktree.display().to_string(),
                    ..Default::default()
                },
                String::new(),
            ))
            .unwrap();
        persist_run_id(&mcp, &item_id, run_id).unwrap();

        mcp.comment_impl(crate::mcp_server::types::CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.clone()),
            body: Some(format!("{REPAIR_MARKER}\n\n{UNIQUE_FINDING}\n")),
            ..Default::default()
        })
        .unwrap();

        let incoming = load_or_synthesize_tasks("work", None);
        let adopt = |owner: &str| {
            let state = crate::workflow::blocking_runtime()
                .block_on(eng.get_status(run_id))
                .unwrap();
            crate::workflow::blocking_runtime()
                .block_on(adopt_existing_run(
                    eng, &mcp, &item_id, run_id, &state, owner, &incoming,
                ))
                .unwrap();
        };
        adopt("claude-code:job-1");

        mcp.comment_impl(crate::mcp_server::types::CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.clone()),
            body: Some(format!("{REPAIR_MARKER}\n\n{UNIQUE_FINDING_2}\n")),
            ..Default::default()
        })
        .unwrap();
        mcp.with_backend_db(|conn| {
            conn.execute(
                "UPDATE item_comments SET created_at = created_at + 10 WHERE item_id = ?1 AND body LIKE ?2",
                rusqlite::params![&item_id, &format!("%{UNIQUE_FINDING_2}%")],
            )
        })
        .unwrap()
        .unwrap();

        adopt("claude-code:job-2");

        let body = crate::workflow::blocking_runtime()
            .block_on(eng.get_status(run_id))
            .unwrap()
            .context
            .data
            .tasks
            .first()
            .map(|t| t.body.clone())
            .unwrap_or_default();
        assert!(
            body.contains(UNIQUE_FINDING_2),
            "newer repair comment must replace task body, got: {body}"
        );
    });
}

#[test]
fn owner_data_patch_does_not_reset_sdd_progress_fields() {
    crate::paths::test_support::with_temp_home(|| {
        let hang_send: flare_workflow::json::SendMessage =
            Arc::new(|_inv| Box::pin(std::future::pending::<Result<(String, u64, u64), String>>()));
        let eng = engine();
        eng.register_workflow(
            flare_workflow::WorkflowDefinition::new(WORKFLOW_ID, "sdd work item")
                .add_step(build_sdd_loop_step(hang_send)),
        )
        .unwrap();
        let run_id = crate::workflow::blocking_runtime()
            .block_on(eng.start_workflow(
                WorkflowId::new(WORKFLOW_ID),
                WorkItemData {
                    agent_name: "implementer-agent".to_string(),
                    judge_agent_name: "judge-agent".to_string(),
                    owner: "claude-code:old".into(),
                    current_task_index: 2,
                    fix_round: 3,
                    review_issues: Some("still open".into()),
                    last_report: Some("prior report".into()),
                    tasks: one_task_data().tasks,
                    ..Default::default()
                },
                String::new(),
            ))
            .unwrap();

        crate::workflow::blocking_runtime()
            .block_on(eng.patch_run_data(run_id, |data: &mut WorkItemData| {
                data.owner = "claude-code:new".into();
            }))
            .unwrap();

        let mut ctx = crate::workflow::blocking_runtime()
            .block_on(eng.get_status(run_id))
            .unwrap()
            .context;
        eng.refresh_registered_data_patch(run_id, &mut ctx.data);

        assert_eq!(ctx.data.current_task_index, 2);
        assert_eq!(ctx.data.fix_round, 3);
        assert_eq!(ctx.data.review_issues.as_deref(), Some("still open"));
        assert_eq!(ctx.data.last_report.as_deref(), Some("prior report"));
        assert_eq!(ctx.data.owner, "claude-code:new");
    });
}
