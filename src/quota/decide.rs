//! The precedence-ordered dispatch decision: given an item, decide whether
//! `run_discovery_tick` should dispatch it, ask a human, wait, self-repair,
//! or (unchanged from today) stay quiet. Pure — reads via `conn`, never
//! writes; `src/supervisor.rs` applies whatever the `Decision` implies.

use super::goal::find_goal_ancestor;
use super::lifecycle::GoalLifecycle;

/// Consecutive self-repairs a goal may run before the next tick is forced
/// to `ask` regardless of what tier 1 would otherwise say.
pub const SELF_REPAIR_CAP: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveAction {
    Run,
    Ask,
    Wait,
    SelfRepair,
    StayQuiet,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Decision {
    pub should_run: bool,
    pub effective_action: EffectiveAction,
    pub reason: String,
    pub gate_question: Option<String>,
}

impl Decision {
    fn run(reason: impl Into<String>) -> Self {
        Decision {
            should_run: true,
            effective_action: EffectiveAction::Run,
            reason: reason.into(),
            gate_question: None,
        }
    }

    fn ask(reason: impl Into<String>, question: impl Into<String>) -> Self {
        Decision {
            should_run: false,
            effective_action: EffectiveAction::Ask,
            reason: reason.into(),
            gate_question: Some(question.into()),
        }
    }

    fn self_repair(reason: impl Into<String>) -> Self {
        Decision {
            should_run: true,
            effective_action: EffectiveAction::SelfRepair,
            reason: reason.into(),
            gate_question: None,
        }
    }

    fn wait(reason: impl Into<String>) -> Self {
        Decision {
            should_run: false,
            effective_action: EffectiveAction::Wait,
            reason: reason.into(),
            gate_question: None,
        }
    }

    fn stay_quiet(reason: impl Into<String>) -> Self {
        Decision {
            should_run: false,
            effective_action: EffectiveAction::StayQuiet,
            reason: reason.into(),
            gate_question: None,
        }
    }

    fn fail_closed(reason: impl Into<String>) -> Self {
        Decision {
            should_run: false,
            effective_action: EffectiveAction::Ask,
            reason: reason.into(),
            gate_question: Some(
                "This goal's saved state is invalid and needs a human to fix it before work can continue.".into(),
            ),
        }
    }
}

pub fn decide(conn: &rusqlite::Connection, item: &agentflare_backend::item::Item) -> Decision {
    let goal = match find_goal_ancestor(conn, item) {
        Ok(g) => g,
        Err(e) => return Decision::fail_closed(format!("goal_state_invalid: {e}")),
    };

    if let Some((goal_item, goal_meta)) = &goal {
        // Tier 1: health-gate.
        if let Ok(vents) = agentflare_backend::vent::list(conn, &goal_item.project_id, true) {
            let linked: Vec<_> = vents
                .iter()
                .filter(|v| {
                    v.item_id.as_deref() == Some(goal_item.id.as_str())
                        || v.item_id.as_deref() == Some(item.id.as_str())
                })
                .collect();
            if !linked.is_empty() {
                let has_high = linked.iter().any(|v| v.severity == "high");
                if has_high {
                    return Decision::ask(
                        "a high-severity friction report is linked to this goal",
                        "An unresolved high-severity friction report is linked to this goal — please review before continuing.",
                    );
                }
                if goal_meta.consecutive_self_repairs >= SELF_REPAIR_CAP {
                    return Decision::ask(
                        format!(
                            "self-repair cap ({SELF_REPAIR_CAP}) reached with friction still linked"
                        ),
                        "This goal has hit its self-repair limit while friction reports remain unresolved — please review.",
                    );
                }
                return Decision::self_repair("low/medium-severity friction linked; repairing under cap");
            }
        }

        // Tier 2: operator-gate.
        if goal_meta.lifecycle == GoalLifecycle::Gated {
            return Decision::ask(
                "goal lifecycle is gated pending a human answer",
                "This goal is waiting on a human answer before it can continue.",
            );
        }
    }

    let now = crate::claims::now();
    let ttl_secs = crate::claims::ttl_secs();

    if let Some((goal_item, _)) = &goal {
        // Tier 3: evidence-wait — a sibling under the same goal is in the
        // "started" state group but its claim has gone stale (heartbeat
        // past TTL): abandoned work needs a human look before this goal
        // takes on anything new.
        if let Ok(siblings) = agentflare_backend::item::list_by_project(conn, &goal_item.project_id) {
            for sibling in siblings
                .iter()
                .filter(|s| s.parent_id.as_deref() == Some(goal_item.id.as_str()) && s.id != item.id)
            {
                let Ok(state) = agentflare_backend::state::get(conn, &sibling.state_id) else {
                    continue;
                };
                if state.group_name != "started" {
                    continue;
                }
                let has_any_claim = agentflare_backend::claim::current_owner(conn, &sibling.id).is_some();
                let has_live_claim = agentflare_backend::claim::has_active_claim_by_other(
                    conn, &sibling.id, "", now, ttl_secs,
                )
                .unwrap_or(false);
                if has_any_claim && !has_live_claim {
                    return Decision::wait(format!(
                        "sibling item {} is started with a stale claim",
                        sibling.id
                    ));
                }
            }
        }
    }

    // Tier 4: focus-wait — another agent already holds a live claim on this
    // item itself.
    let this_owner = item.assignee_agent.as_deref().unwrap_or("");
    if agentflare_backend::claim::has_active_claim_by_other(conn, &item.id, this_owner, now, ttl_secs)
        .unwrap_or(false)
    {
        return Decision::wait("another agent already holds a live claim on this item");
    }

    // Tiers 5-6 land in Task 5.
    Decision::run("stub: tiers 5-6 not yet implemented")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quota::goal::{GoalMetadata, GoalScope};
    use crate::quota::lifecycle::GoalLifecycle;

    fn test_conn() -> rusqlite::Connection {
        agentflare_backend::db::open_in_memory().unwrap()
    }

    fn seed_project(conn: &rusqlite::Connection) -> (String, String) {
        let workspace = agentflare_backend::workspace::create(
            conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "ws".into(),
                slug: "ws".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let project = agentflare_backend::project::create(
            conn,
            agentflare_backend::project::CreateProject {
                workspace_id: workspace.id.clone(),
                name: "proj".into(),
                identifier: "proj".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        let states = agentflare_backend::state::list_by_project(conn, &project.id).unwrap();
        let state_id = states.iter().find(|s| s.is_default).unwrap().id.clone();
        (project.id, state_id)
    }

    fn make_goal_item(
        conn: &rusqlite::Connection,
        project_id: &str,
        state_id: &str,
        lifecycle: GoalLifecycle,
        consecutive_self_repairs: u32,
    ) -> agentflare_backend::item::Item {
        let goal = GoalMetadata {
            objective: "ship it".into(),
            scope: GoalScope::default(),
            quota_mode: "default".into(),
            lifecycle,
            consecutive_self_repairs,
        };
        let metadata = serde_json::json!({ "goal": goal }).to_string();
        agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project_id.into(),
                state_id: state_id.into(),
                name: "goal".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: Some(metadata),
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap()
    }

    fn make_todo(
        conn: &rusqlite::Connection,
        project_id: &str,
        state_id: &str,
        parent_id: &str,
    ) -> agentflare_backend::item::Item {
        agentflare_backend::item::create(
            conn,
            agentflare_backend::item::CreateItem {
                project_id: project_id.into(),
                state_id: state_id.into(),
                name: "todo".into(),
                description: None,
                priority: None,
                parent_id: Some(parent_id.into()),
                assignee_agent: Some("claude-code".into()),
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: None,
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap()
    }

    #[test]
    fn high_severity_vent_on_goal_forces_ask() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);
        agentflare_backend::vent::upsert(
            &conn, &pid, "something broke", "high", "[]", "topic", "evt-1", 1,
            crate::claims::now(),
        )
        .unwrap();
        let vents = agentflare_backend::vent::list(&conn, &pid, false).unwrap();
        agentflare_backend::vent::set_actionable(&conn, &vents[0].id, true).unwrap();
        agentflare_backend::vent::link_item(&conn, &vents[0].id, &goal.id).unwrap();

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveAction::Ask);
        assert!(!decision.should_run);
        assert!(decision.gate_question.is_some());
    }

    #[test]
    fn low_severity_vent_under_cap_self_repairs() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);
        agentflare_backend::vent::upsert(
            &conn, &pid, "minor friction", "low", "[]", "topic", "evt-1", 1,
            crate::claims::now(),
        )
        .unwrap();
        let vents = agentflare_backend::vent::list(&conn, &pid, false).unwrap();
        agentflare_backend::vent::set_actionable(&conn, &vents[0].id, true).unwrap();
        agentflare_backend::vent::link_item(&conn, &vents[0].id, &goal.id).unwrap();

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveAction::SelfRepair);
    }

    #[test]
    fn low_severity_vent_at_cap_forces_ask() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, SELF_REPAIR_CAP);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);
        agentflare_backend::vent::upsert(
            &conn, &pid, "minor friction", "low", "[]", "topic", "evt-1", 1,
            crate::claims::now(),
        )
        .unwrap();
        let vents = agentflare_backend::vent::list(&conn, &pid, false).unwrap();
        agentflare_backend::vent::set_actionable(&conn, &vents[0].id, true).unwrap();
        agentflare_backend::vent::link_item(&conn, &vents[0].id, &goal.id).unwrap();

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveAction::Ask);
    }

    #[test]
    fn gated_lifecycle_forces_ask_even_with_no_vent() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Gated, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveAction::Ask);
    }

    #[test]
    fn active_lifecycle_with_no_vent_does_not_ask() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);

        let decision = decide(&conn, &todo);
        assert_ne!(decision.effective_action, EffectiveAction::Ask);
    }

    fn seed_started_state(conn: &rusqlite::Connection, project_id: &str) -> String {
        let states = agentflare_backend::state::list_by_project(conn, project_id).unwrap();
        states
            .iter()
            .find(|s| s.group_name == "started")
            .unwrap()
            .id
            .clone()
    }

    #[test]
    fn stale_claim_on_sibling_waits() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, 0);
        let started_sid = seed_started_state(&conn, &pid);
        let stalled_sibling = make_todo(&conn, &pid, &started_sid, &goal.id);
        // Claim it, then let the TTL be zero seconds — instantly stale.
        agentflare_backend::claim::acquire(&conn, &stalled_sibling.id, "claude:1", crate::claims::now() - 10_000, 1)
            .unwrap();
        let todo = make_todo(&conn, &pid, &sid, &goal.id);

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveAction::Wait);
    }

    #[test]
    fn live_claim_by_another_agent_on_this_item_waits() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);
        agentflare_backend::claim::acquire(&conn, &todo.id, "codex:1", crate::claims::now(), 1800)
            .unwrap();

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveAction::Wait);
    }

    #[test]
    fn no_claims_anywhere_falls_through_past_tiers_3_and_4() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);

        let decision = decide(&conn, &todo);
        assert_ne!(decision.effective_action, EffectiveAction::Wait);
    }
}
