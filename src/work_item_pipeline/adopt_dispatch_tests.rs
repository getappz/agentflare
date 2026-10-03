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
        let eng = engine();
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
