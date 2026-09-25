//! Operator pause/resume/cancel of a dispatched work item, end to end
//! through `run_or_resume_with_sender`: a pause stops the job on request
//! but leaves the run paused (worktree kept, claim released, item labeled
//! `paused`); a resume re-arms the item and the next dispatch continues the
//! same run to completion. A cancel is terminal.
use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

fn is_judge(prompt: &str) -> bool {
    prompt.contains("You are the judge")
}

/// Implementer turns block until `cancel` is set, then fail the way the real
/// send hook does when its job is cancelled.
fn blocking_send(
    entered: Arc<AtomicU32>,
    cancel: Arc<AtomicBool>,
) -> flare_workflow::json::SendMessage {
    Arc::new(move |inv: flare_workflow::json::StepInvocation| {
        let entered = entered.clone();
        let cancel = cancel.clone();
        Box::pin(async move {
            if is_judge(&inv.prompt) {
                return Err("judge must not run while the turn is blocked".to_string());
            }
            entered.fetch_add(1, Ordering::SeqCst);
            while !cancel.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(agentflare_jobs::cancel::CANCELLED_MESSAGE.to_string())
        })
    })
}

fn completing_send(implementer_calls: Arc<AtomicU32>) -> flare_workflow::json::SendMessage {
    Arc::new(move |inv: flare_workflow::json::StepInvocation| {
        let implementer_calls = implementer_calls.clone();
        Box::pin(async move {
            if is_judge(&inv.prompt) {
                Ok((
                    r#"{"action":"complete_pipeline","rationale":"done","ledger_line":"Task 0: complete","task_model_tier":null}"#
                        .to_string(),
                    1u64,
                    0u64,
                ))
            } else {
                implementer_calls.fetch_add(1, Ordering::SeqCst);
                Ok(("DONE: did the work".to_string(), 1u64, 0u64))
            }
        })
    })
}

fn wait_until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..1000 {
        if f() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

fn get_item(mcp: &AgentflareMcp, item_id: &str) -> agentflare_backend::item::Item {
    mcp.with_backend_db(|conn| agentflare_backend::item::get(conn, item_id).unwrap())
        .unwrap()
}

fn has_label(mcp: &AgentflareMcp, item_id: &str, name: &str) -> bool {
    mcp.with_backend_db(|conn| {
        let item = agentflare_backend::item::get(conn, item_id).unwrap();
        let Some(label) = agentflare_backend::label::list_by_project(conn, &item.project_id)
            .unwrap()
            .into_iter()
            .find(|l| l.name == name)
        else {
            return false;
        };
        agentflare_backend::item::list_labels(conn, item_id)
            .unwrap()
            .contains(&label.id)
    })
    .unwrap()
}

/// A daemon-job-shaped claim owner `<agent>:<job-id>` unique to this test,
/// so the cancel check registered under its job id can't reach any other
/// test's run. The agent part matches the item's claimed assignee.
fn job_owner(job: &str) -> (String, String) {
    let agent = crate::claims::agent_of(&crate::claims::owner_id()).to_string();
    let job_id = format!("{job}-{}", std::process::id());
    (format!("{agent}:{job_id}"), job_id)
}

/// Starts a dispatch on its own thread (`run_or_resume` blocks on the
/// workflow runtime) and waits for its implementer turn to be in flight.
fn dispatch_blocked(
    owner: &str,
    mcp: &Arc<AgentflareMcp>,
    item_id: &str,
    worktree: &std::path::Path,
    cancel: &Arc<AtomicBool>,
) -> std::thread::JoinHandle<Result<(), String>> {
    let entered = Arc::new(AtomicU32::new(0));
    let send = blocking_send(entered.clone(), cancel.clone());
    let item = get_item(mcp, item_id);
    let (mcp, worktree, owner) = (mcp.clone(), worktree.to_path_buf(), owner.to_string());
    let handle = std::thread::spawn(move || {
        crate::claims::with_owner_override(owner, || {
            run_or_resume_with_sender(
                mcp,
                &item,
                &worktree,
                agent_registry::Agent::ClaudeCode,
                agent_registry::Agent::ClaudeCode,
                "implement it".to_string(),
                None,
                None,
                send,
            )
        })
    });
    wait_until("the implementer turn to start", || {
        entered.load(Ordering::SeqCst) == 1
    });
    handle
}

#[test]
fn pause_then_resume_continues_the_same_run_in_the_same_worktree() {
    crate::paths::test_support::with_temp_home(|| {
        let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, worktree) =
            crate::mcp_server::tests::mcp_with_claimed_item("Pause/resume round trip");
        std::fs::write(worktree.join("real_work.txt"), "real work").unwrap();
        let mcp = Arc::new(mcp);
        // The job cancel flag the real `WorkerPool` registers under the
        // owner's instance id; the pause cancels the job, which is what
        // kills the in-flight agent turn.
        let (owner, instance) = job_owner("pause-test-job");
        let cancel = Arc::new(AtomicBool::new(false));
        let _job = agentflare_jobs::cancel::register(&instance, {
            let cancel = cancel.clone();
            move || cancel.load(Ordering::SeqCst)
        });

        let first = dispatch_blocked(&owner, &mcp, &item_id, &worktree, &cancel);
        let run_id = crate::job_controls::run_id_of(&get_item(&mcp, &item_id).metadata)
            .expect("run id persisted");
        let paused: serde_json::Value = serde_json::from_str(
            &mcp.item_pause(ItemRequest {
                action: "pause".into(),
                id: Some(item_id.clone()),
                reason: Some("operator break".into()),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(paused["run_paused"], true, "{paused}");
        cancel.store(true, Ordering::SeqCst);

        let err = first.join().unwrap().expect_err("the paused job stops");
        assert!(
            err.contains(crate::cli::work::PAUSED_MESSAGE),
            "a pause ends the job as a stop on request, not a failure: {err}"
        );
        assert_eq!(
            crate::job_controls::run_status(run_id),
            Some(WorkflowStatus::Paused)
        );
        assert!(worktree.join("real_work.txt").exists(), "worktree kept");
        assert!(has_label(&mcp, &item_id, crate::supervisor::PAUSED_LABEL));
        assert!(
            mcp.with_backend_db(|conn| agentflare_backend::claim::current_owner(conn, &item_id))
                .unwrap()
                .is_none(),
            "the claim is released while paused"
        );

        // Resume, retrying while the paused driver finishes winding down.
        // Bounded by wall-clock, not by a poll count: each refused attempt
        // costs several backend queries, and under a fully parallel nextest
        // run those (and the driver's own drain) run far slower than on an
        // idle machine -- a fixed 200 x 20ms budget expired there every time
        // while the driver was still letting go. A driver that never lets
        // go still fails the test, just later.
        cancel.store(false, Ordering::SeqCst);
        let mut resumed = None;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while std::time::Instant::now() < deadline {
            match mcp.item_resume(ItemRequest {
                action: "resume".into(),
                id: Some(item_id.clone()),
                ..Default::default()
            }) {
                Ok(json) => {
                    resumed = Some(json);
                    break;
                }
                Err(e) if e.message.contains("still stopping") => {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                Err(e) => panic!("resume failed: {e:?}"),
            }
        }
        let resumed: serde_json::Value = serde_json::from_str(
            &resumed.expect("the paused driver let go of the run within the deadline"),
        )
        .unwrap();
        assert_eq!(resumed["continues_run"], true, "{resumed}");
        assert!(!has_label(&mcp, &item_id, crate::supervisor::PAUSED_LABEL));

        // The next dispatch re-claims (as `execute_work` does) and adopts the
        // paused run instead of starting a new one.
        let (next_owner, _) = job_owner("pause-test-next-job");
        crate::claims::with_owner_override(next_owner.clone(), || {
            mcp.item_claim(ItemRequest {
                action: "claim".into(),
                id: Some(item_id.clone()),
                ..Default::default()
            })
        })
        .unwrap();
        let implementer_calls = Arc::new(AtomicU32::new(0));
        let item = get_item(&mcp, &item_id);
        let result = crate::claims::with_owner_override(next_owner, || {
            run_or_resume_with_sender(
                mcp.clone(),
                &item,
                &worktree,
                agent_registry::Agent::ClaudeCode,
                agent_registry::Agent::ClaudeCode,
                "implement it".to_string(),
                None,
                None,
                completing_send(implementer_calls.clone()),
            )
        });
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(
            crate::job_controls::run_id_of(&get_item(&mcp, &item_id).metadata),
            Some(run_id),
            "the same run continued"
        );
        assert_eq!(
            crate::job_controls::run_status(run_id),
            Some(WorkflowStatus::Completed)
        );
        assert_eq!(implementer_calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn cancel_stops_the_running_job_and_its_run_for_good() {
    crate::paths::test_support::with_temp_home(|| {
        let (mcp, _backend_tmp, _repo_tmp, item_id, _project_id, worktree) =
            crate::mcp_server::tests::mcp_with_claimed_item("Cancel is terminal");
        let mcp = Arc::new(mcp);
        let (owner, instance) = job_owner("cancel-test-job");
        let cancel = Arc::new(AtomicBool::new(false));
        let _job = agentflare_jobs::cancel::register(&instance, {
            let cancel = cancel.clone();
            move || cancel.load(Ordering::SeqCst)
        });

        let running = dispatch_blocked(&owner, &mcp, &item_id, &worktree, &cancel);
        let run_id = crate::job_controls::run_id_of(&get_item(&mcp, &item_id).metadata).unwrap();
        let cancelled: serde_json::Value = serde_json::from_str(
            &mcp.item_control(ItemRequest {
                action: "cancel".into(),
                id: Some(item_id.clone()),
                reason: Some("no longer needed".into()),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
        assert_eq!(cancelled["run_cancelled"], true, "{cancelled}");
        // What `Queue::cancel_for_item` does to the running job inside the
        // cancel (this test's MCP has no queue to reach): its agent is killed.
        cancel.store(true, Ordering::SeqCst);

        let err = running
            .join()
            .unwrap()
            .expect_err("the cancelled job stops");
        assert!(
            err.contains(agentflare_jobs::cancel::CANCELLED_MESSAGE),
            "reported as a deliberate stop (fatal, never retried): {err}"
        );
        assert_eq!(
            crate::job_controls::run_status(run_id),
            Some(WorkflowStatus::Cancelled)
        );
        // Nothing can bring it back: resume refuses, and a later dispatch
        // would not adopt a cancelled run.
        assert!(
            mcp.item_resume(ItemRequest {
                action: "resume".into(),
                id: Some(item_id.clone()),
                ..Default::default()
            })
            .is_err()
        );
        let group = mcp
            .with_backend_db(|conn| {
                let item = agentflare_backend::item::get(conn, &item_id).unwrap();
                agentflare_backend::state::get(conn, &item.state_id)
                    .unwrap()
                    .group_name
            })
            .unwrap();
        assert_eq!(group, "cancelled");
        assert!(!has_label(&mcp, &item_id, crate::supervisor::READY_LABEL));
    });
}
