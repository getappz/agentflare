//! Background discovery loop: finds items labeled `ready-for-work` and
//! dispatches an `agentflare work` job for each one whose assignee is a
//! confirmed-autonomous agent (skips the rest with a comment).

use crate::mcp_server::types::{CommentRequest, ItemRequest};
use crate::mcp_server::AgentflareMcp;

const READY_LABEL: &str = "ready-for-work";
const DISPATCHED_LABEL: &str = "dispatched";
const NEEDS_MANUAL_LABEL: &str = "needs-manual-dispatch";

/// Returns the matching `Agent` only if `agent_registry::autonomous_args`
/// confirms it has a headless permission-bypass flag — the same gate
/// `agentflare work` itself uses (`src/cli/work.rs`'s `run_work`).
pub(crate) fn resolve_confirmed_agent(assignee: &str) -> Option<agent_registry::Agent> {
    let agent = agent_registry::REGISTRY
        .iter()
        .find(|s| s.id.as_str() == assignee)
        .map(|s| s.id)?;
    agent_registry::autonomous_args(agent).map(|_| agent)
}

pub(crate) struct DiscoveryTickResult {
    pub dispatched: usize,
    pub skipped: usize,
}

/// One pass: list items labeled `ready-for-work`, dispatch a job for each
/// one with a confirmed-autonomous assignee, skip (+ comment + relabel) the
/// rest. Ends after enqueueing — it does not watch job completion, since
/// `agentflare work` itself reports outcome back onto the item.
pub(crate) fn run_discovery_tick(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
) -> DiscoveryTickResult {
    let mut result = DiscoveryTickResult {
        dispatched: 0,
        skipped: 0,
    };

    let fetched = mcp.with_backend_db(|conn| {
        let project = mcp.resolve_project(conn).ok()?;
        let labels = agentflare_backend::label::list_by_project(conn, &project.id).ok()?;
        let mut label_id_by_name = std::collections::HashMap::new();
        for l in &labels {
            label_id_by_name.insert(l.name.clone(), l.id.clone());
        }
        let ready_id = label_id_by_name.get(READY_LABEL)?.clone();
        let items = agentflare_backend::item::list_by_label(conn, &project.id, &ready_id).ok()?;
        Some((items, label_id_by_name))
    });

    let Ok(Some((items, label_id_by_name))) = fetched else {
        return result;
    };
    let Some(ready_id) = label_id_by_name.get(READY_LABEL).cloned() else {
        return result;
    };

    for item in items {
        match item.assignee_agent.as_deref().and_then(resolve_confirmed_agent) {
            None => {
                skip_item(mcp, &item, &label_id_by_name, &ready_id);
                result.skipped += 1;
            }
            Some(agent) => {
                if dispatch_item(mcp, queue, &item, agent, &label_id_by_name, &ready_id) {
                    result.dispatched += 1;
                }
            }
        }
    }
    result
}

fn skip_item(
    mcp: &AgentflareMcp,
    item: &agentflare_backend::item::Item,
    label_id_by_name: &std::collections::HashMap<String, String>,
    ready_id: &str,
) {
    let reason = match &item.assignee_agent {
        None => "no assignee_agent set — cannot auto-dispatch".to_string(),
        Some(a) => format!("assignee '{a}' is not a confirmed-autonomous agent"),
    };
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item.id.clone()),
        body: Some(format!(
            "## supervisor — skipped\n\n{reason}. Run `agentflare work` manually."
        )),
        ..Default::default()
    });
    let _ = mcp.item_remove_label(ItemRequest {
        action: "remove_label".into(),
        id: Some(item.id.clone()),
        label_id: Some(ready_id.to_string()),
        ..Default::default()
    });
    if let Some(needs_manual_id) = label_id_by_name.get(NEEDS_MANUAL_LABEL) {
        let _ = mcp.item_add_label(ItemRequest {
            action: "add_label".into(),
            id: Some(item.id.clone()),
            label_id: Some(needs_manual_id.clone()),
            ..Default::default()
        });
    }
}

fn dispatch_item(
    mcp: &AgentflareMcp,
    queue: &agentflare_jobs::Queue,
    item: &agentflare_backend::item::Item,
    agent: agent_registry::Agent,
    label_id_by_name: &std::collections::HashMap<String, String>,
    ready_id: &str,
) -> bool {
    let command = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| "agentflare".to_string());
    let job = agentflare_jobs::AgentJob::new(command).args([
        "work".to_string(),
        item.id.clone(),
        "--agent".to_string(),
        agent.as_str().to_string(),
    ]);
    let Ok(info) = queue.enqueue(&job) else {
        return false;
    };

    let _ = mcp.item_remove_label(ItemRequest {
        action: "remove_label".into(),
        id: Some(item.id.clone()),
        label_id: Some(ready_id.to_string()),
        ..Default::default()
    });
    if let Some(dispatched_id) = label_id_by_name.get(DISPATCHED_LABEL) {
        let _ = mcp.item_add_label(ItemRequest {
            action: "add_label".into(),
            id: Some(item.id.clone()),
            label_id: Some(dispatched_id.clone()),
            ..Default::default()
        });
    }
    let _ = mcp.comment_impl(CommentRequest {
        action: "create".into(),
        item_id: Some(item.id.clone()),
        body: Some(format!("## supervisor — dispatched\n\njob: {}", info.id)),
        ..Default::default()
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_confirmed_agent_accepts_claude_code() {
        assert_eq!(
            resolve_confirmed_agent("claude-code"),
            Some(agent_registry::Agent::ClaudeCode)
        );
    }

    #[test]
    fn resolve_confirmed_agent_rejects_opencode() {
        assert_eq!(resolve_confirmed_agent("opencode"), None);
    }

    #[test]
    fn resolve_confirmed_agent_rejects_unknown_agent_string() {
        assert_eq!(resolve_confirmed_agent("not-a-real-agent"), None);
    }

    fn test_mcp() -> AgentflareMcp {
        AgentflareMcp::for_test_memory()
    }

    fn test_queue() -> agentflare_jobs::Queue {
        let dir = tempfile::tempdir().unwrap().keep();
        agentflare_jobs::Queue::open_memory(dir.join("logs")).unwrap()
    }

    fn seed_ready_item(mcp: &AgentflareMcp, assignee: Option<&str>) -> String {
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            for name in ["ready-for-work", "dispatched", "needs-manual-dispatch"] {
                agentflare_backend::label::create(
                    conn,
                    agentflare_backend::label::CreateLabel {
                        project_id: Some(project.id.clone()),
                        workspace_id: project.workspace_id.clone(),
                        name: name.into(),
                        color: None,
                        parent_id: None,
                        sort_order: None,
                        external_source: None,
                        external_id: None,
                    },
                )
                .unwrap();
            }
            let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
            let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
            let item = agentflare_backend::item::create(
                conn,
                agentflare_backend::item::CreateItem {
                    project_id: project.id.clone(),
                    state_id,
                    name: "Do the thing".into(),
                    description: Some("do it well".into()),
                    priority: None,
                    parent_id: None,
                    assignee_agent: assignee.map(str::to_string),
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                    metadata: None,
                    label_ids: vec![],
                    assignee_ids: vec![],
                    dependency_ids: vec![],
                },
            )
            .unwrap();
            let labels = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
            let ready_id = &labels.iter().find(|l| l.name == "ready-for-work").unwrap().id;
            agentflare_backend::item::add_label(conn, &item.id, ready_id).unwrap();
            item.id
        })
        .unwrap()
    }

    fn labels_contain_name(mcp: &AgentflareMcp, label_ids: &[String], name: &str) -> bool {
        mcp.with_backend_db(|conn| {
            let project = mcp.resolve_project(conn).unwrap();
            let all = agentflare_backend::label::list_by_project(conn, &project.id).unwrap();
            let target = all.iter().find(|l| l.name == name).unwrap();
            label_ids.contains(&target.id)
        })
        .unwrap()
    }

    #[test]
    fn confirmed_agent_gets_dispatched_and_relabeled() {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_ready_item(&mcp, Some("claude-code"));

        let result = run_discovery_tick(&mcp, &queue);

        assert_eq!(result.dispatched, 1);
        assert_eq!(result.skipped, 0);

        let jobs = queue.list(None).unwrap();
        assert_eq!(jobs.len(), 1);
        assert!(jobs[0].args.contains(&"work".to_string()));
        assert!(jobs[0].args.contains(&item_id));
        assert!(jobs[0].args.contains(&"claude-code".to_string()));

        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(!labels_contain_name(&mcp, &labels, "ready-for-work"));
        assert!(labels_contain_name(&mcp, &labels, "dispatched"));
    }

    #[test]
    fn unconfirmed_agent_gets_skipped_not_dispatched() {
        let mcp = test_mcp();
        let queue = test_queue();
        let item_id = seed_ready_item(&mcp, Some("opencode"));

        let result = run_discovery_tick(&mcp, &queue);

        assert_eq!(result.dispatched, 0);
        assert_eq!(result.skipped, 1);
        assert!(queue.list(None).unwrap().is_empty());

        let labels = mcp
            .with_backend_db(|conn| agentflare_backend::item::list_labels(conn, &item_id).unwrap())
            .unwrap();
        assert!(!labels_contain_name(&mcp, &labels, "ready-for-work"));
        assert!(labels_contain_name(&mcp, &labels, "needs-manual-dispatch"));
    }
}
