//! Shared fixtures for `sdd_loop_tests` (this task) and later plan tasks'
//! test modules (Task 6's `cap_tests`, Tasks 11/12) that need the same
//! mocked `send` and a minimal single-task `WorkItemData` — a sibling
//! `#[cfg(test)] mod` can't reach into another sibling module's private
//! items, so these live in their own module and get pulled in via
//! `use super::sdd_test_support::*;`.
use super::*;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Records every `(agent_name, prompt, args)` call and returns queued
/// replies in order.
#[allow(clippy::type_complexity)]
pub(crate) fn mock_send(
    replies: Vec<&'static str>,
) -> (
    flare_workflow::json::SendMessage,
    Arc<Mutex<Vec<(String, String, Vec<String>)>>>,
) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(replies.into_iter().collect::<VecDeque<_>>()));
    let calls_clone = calls.clone();
    let send: flare_workflow::json::SendMessage =
        Arc::new(move |inv: flare_workflow::json::StepInvocation| {
            calls_clone.lock().unwrap().push((
                inv.agent.clone(),
                inv.prompt.clone(),
                inv.args.clone(),
            ));
            let reply = queue.lock().unwrap().pop_front().unwrap_or("").to_string();
            Box::pin(async move { Ok((reply, 10u64, 10u64)) })
        });
    (send, calls)
}

pub(crate) fn one_task_data() -> WorkItemData {
    WorkItemData {
        agent_name: "implementer-agent".to_string(),
        judge_agent_name: "judge-agent".to_string(),
        tasks: vec![SddTask {
            id: 0,
            title: "Add flag".to_string(),
            body: "Add --verbose".to_string(),
            model_tier: None,
        }],
        ..Default::default()
    }
}

/// `build_sdd_loop_step` — `one_task_data()`'s `agent_name`/`judge_agent_name`
/// supply the fixed agent names `sdd_loop_tests` uses (read from `ctx.data`,
/// not passed here — see `build_sdd_loop_step`'s own doc comment).
pub(crate) fn sdd_step(send: flare_workflow::json::SendMessage) -> StepDefinition<WorkItemData> {
    build_sdd_loop_step(send)
}
