//! The precedence-ordered dispatch decision: given an item, decide whether
//! `run_discovery_tick` should dispatch it, ask a human, wait, self-repair,
//! or (unchanged from today) stay quiet. Pure — reads via `conn`, never
//! writes; `src/supervisor.rs` applies whatever the `Decision` implies.

use super::goal::find_goal_ancestor;
use super::lifecycle::GoalLifecycle;
use crate::vent::classify::severity_rank;

/// Consecutive self-repairs a goal may run before the next tick is forced
/// to `ask` regardless of what tier 1 would otherwise say.
pub const SELF_REPAIR_CAP: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectiveActionInternal {
    Run,
    Ask,
    Wait,
    SelfRepair,
    StayQuiet,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Decision {
    pub should_run: bool,
    pub effective_action: EffectiveActionInternal,
    pub reason: String,
    pub gate_question: Option<String>,
}

impl Decision {
    fn run(reason: impl Into<String>) -> Self {
        Decision {
            should_run: true,
            effective_action: EffectiveActionInternal::Run,
            reason: reason.into(),
            gate_question: None,
        }
    }

    fn ask(reason: impl Into<String>, question: impl Into<String>) -> Self {
        Decision {
            should_run: false,
            effective_action: EffectiveActionInternal::Ask,
            reason: reason.into(),
            gate_question: Some(question.into()),
        }
    }

    fn self_repair(reason: impl Into<String>) -> Self {
        Decision {
            should_run: true,
            effective_action: EffectiveActionInternal::SelfRepair,
            reason: reason.into(),
            gate_question: None,
        }
    }

    fn wait(reason: impl Into<String>) -> Self {
        Decision {
            should_run: false,
            effective_action: EffectiveActionInternal::Wait,
            reason: reason.into(),
            gate_question: None,
        }
    }

    fn stay_quiet(reason: impl Into<String>) -> Self {
        Decision {
            should_run: false,
            effective_action: EffectiveActionInternal::StayQuiet,
            reason: reason.into(),
            gate_question: None,
        }
    }

    fn fail_closed(reason: impl Into<String>) -> Self {
        Decision {
            should_run: false,
            effective_action: EffectiveActionInternal::Ask,
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
                let has_high = linked
                    .iter()
                    .any(|v| severity_rank(&v.severity) >= severity_rank("high"));
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
                return Decision::self_repair(
                    "low/medium-severity friction linked; repairing under cap",
                );
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
        if let Ok(siblings) = agentflare_backend::item::list_by_project(conn, &goal_item.project_id)
        {
            for sibling in siblings.iter().filter(|s| {
                s.parent_id.as_deref() == Some(goal_item.id.as_str()) && s.id != item.id
            }) {
                let Ok(state) = agentflare_backend::state::get(conn, &sibling.state_id) else {
                    continue;
                };
                if state.group_name != "started" {
                    continue;
                }
                let has_any_claim =
                    agentflare_backend::claim::current_owner(conn, &sibling.id).is_some();
                let has_live_claim = agentflare_backend::claim::has_active_claim_by_other(
                    conn,
                    &sibling.id,
                    "",
                    now,
                    ttl_secs,
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
    if agentflare_backend::claim::has_active_claim_by_other(
        conn, &item.id, this_owner, now, ttl_secs,
    )
    .unwrap_or(false)
    {
        return Decision::wait("another agent already holds a live claim on this item");
    }

    // Tier 5: eligibility — unchanged from today's supervisor behavior.
    if crate::supervisor::resolve_confirmed_agent(item.assignee_agent.as_deref().unwrap_or(""))
        .is_none()
    {
        return Decision::stay_quiet(match &item.assignee_agent {
            None => "no assignee_agent set — cannot auto-dispatch".to_string(),
            Some(a) => format!("assignee '{a}' is not a confirmed-autonomous agent"),
        });
    }

    // Tier 6: compute-quota. Stub for v1 — always passes. Real budget
    // enforcement (a port of agenticmq::limiter's sliding-window TPM/RPM
    // logic) is a follow-on spec; this is its interface point.
    Decision::run("eligible and nothing blocking")
}

/// Caller-facing dispatch decision for `run_discovery_tick`'s match:
/// `Decision`'s tier-level result is folded so the `Ask` variant carries
/// the gate question inline, and the goal lifecycle transition / self-repair
/// counter side effects that `decide()` itself (being side-effect-free)
/// deliberately does not perform are applied here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectiveAction {
    Run,
    SelfRepair,
    Wait,
    Ask(String),
    StayQuiet,
}

pub fn decide_for_supervisor(
    mcp: &crate::mcp_server::AgentflareMcp,
    item: &agentflare_backend::item::Item,
) -> EffectiveAction {
    let now = crate::claims::now();
    let decision = mcp
        .with_backend_db(|conn| decide(conn, item))
        .unwrap_or_else(|_| Decision::fail_closed("could not open backend db"));

    let goal = mcp
        .with_backend_db(|conn| super::goal::find_goal_ancestor(conn, item))
        .ok()
        .and_then(Result::ok)
        .flatten();

    match decision.effective_action {
        EffectiveActionInternal::Run => EffectiveAction::Run,
        EffectiveActionInternal::SelfRepair => {
            if let Some((goal_item, mut meta)) = goal {
                meta.consecutive_self_repairs += 1;
                let _ = mcp.with_backend_db(|conn| {
                    super::goal::save_goal_metadata(conn, &goal_item.id, &meta)
                });
            }
            EffectiveAction::SelfRepair
        }
        EffectiveActionInternal::Wait => EffectiveAction::Wait,
        EffectiveActionInternal::Ask => {
            let goal_item_id = goal.as_ref().map(|(gi, _)| gi.id.clone());
            if let Some((goal_item, mut meta)) = goal {
                meta.consecutive_self_repairs = 0;
                if let Ok(next) = meta.lifecycle.apply(super::lifecycle::LifecycleEvent::Gate) {
                    meta.lifecycle = next;
                }
                let _ = mcp.with_backend_db(|conn| {
                    super::goal::save_goal_metadata(conn, &goal_item.id, &meta)
                });
            }
            let _ = mcp.with_backend_db(|conn| {
                agentflare_backend::ask_event::record(
                    conn,
                    &item.project_id,
                    goal_item_id.as_deref(),
                    &item.id,
                    item.assignee_agent.as_deref(),
                    &decision.reason,
                    decision.gate_question.as_deref(),
                    now,
                )
            });
            EffectiveAction::Ask(
                decision
                    .gate_question
                    .unwrap_or_else(|| decision.reason.clone()),
            )
        }
        EffectiveActionInternal::StayQuiet => EffectiveAction::StayQuiet,
    }
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
            &conn,
            &pid,
            "something broke",
            "high",
            "[]",
            "topic",
            "evt-1",
            1,
            crate::claims::now(),
        )
        .unwrap();
        let vents = agentflare_backend::vent::list(&conn, &pid, false).unwrap();
        agentflare_backend::vent::set_actionable(&conn, &vents[0].id, true).unwrap();
        agentflare_backend::vent::link_item(&conn, &vents[0].id, &goal.id).unwrap();

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveActionInternal::Ask);
        assert!(!decision.should_run);
        assert!(decision.gate_question.is_some());
    }

    #[test]
    fn critical_severity_vent_on_goal_forces_ask() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);
        agentflare_backend::vent::upsert(
            &conn,
            &pid,
            "everything is on fire",
            "critical",
            "[]",
            "topic",
            "evt-1",
            1,
            crate::claims::now(),
        )
        .unwrap();
        let vents = agentflare_backend::vent::list(&conn, &pid, false).unwrap();
        agentflare_backend::vent::set_actionable(&conn, &vents[0].id, true).unwrap();
        agentflare_backend::vent::link_item(&conn, &vents[0].id, &goal.id).unwrap();

        let decision = decide(&conn, &todo);
        assert_eq!(
            decision.effective_action,
            EffectiveActionInternal::Ask,
            "critical must gate exactly like high, not fall through to self-repair"
        );
        assert!(!decision.should_run);
    }

    #[test]
    fn low_severity_vent_under_cap_self_repairs() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);
        agentflare_backend::vent::upsert(
            &conn,
            &pid,
            "minor friction",
            "low",
            "[]",
            "topic",
            "evt-1",
            1,
            crate::claims::now(),
        )
        .unwrap();
        let vents = agentflare_backend::vent::list(&conn, &pid, false).unwrap();
        agentflare_backend::vent::set_actionable(&conn, &vents[0].id, true).unwrap();
        agentflare_backend::vent::link_item(&conn, &vents[0].id, &goal.id).unwrap();

        let decision = decide(&conn, &todo);
        assert_eq!(
            decision.effective_action,
            EffectiveActionInternal::SelfRepair
        );
    }

    #[test]
    fn low_severity_vent_at_cap_forces_ask() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, SELF_REPAIR_CAP);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);
        agentflare_backend::vent::upsert(
            &conn,
            &pid,
            "minor friction",
            "low",
            "[]",
            "topic",
            "evt-1",
            1,
            crate::claims::now(),
        )
        .unwrap();
        let vents = agentflare_backend::vent::list(&conn, &pid, false).unwrap();
        agentflare_backend::vent::set_actionable(&conn, &vents[0].id, true).unwrap();
        agentflare_backend::vent::link_item(&conn, &vents[0].id, &goal.id).unwrap();

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveActionInternal::Ask);
    }

    #[test]
    fn gated_lifecycle_forces_ask_even_with_no_vent() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Gated, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveActionInternal::Ask);
    }

    #[test]
    fn gated_lifecycle_records_an_ask_event() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Gated, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveActionInternal::Ask);

        // decide() itself is pure and does not write; recording happens in
        // decide_for_supervisor, which needs an AgentflareMcp and is covered
        // by ask_event's own unit test for the write path. This pins that
        // decide() still reaches Ask for a gated goal, which
        // decide_for_supervisor's Ask arm relies on to call
        // ask_event::record.
        assert!(decision.gate_question.is_some());
    }

    #[test]
    fn active_lifecycle_with_no_vent_does_not_ask() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);

        let decision = decide(&conn, &todo);
        assert_ne!(decision.effective_action, EffectiveActionInternal::Ask);
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
        agentflare_backend::claim::acquire(
            &conn,
            &stalled_sibling.id,
            "claude:1",
            crate::claims::now() - 10_000,
            1,
        )
        .unwrap();
        let todo = make_todo(&conn, &pid, &sid, &goal.id);

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveActionInternal::Wait);
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
        assert_eq!(decision.effective_action, EffectiveActionInternal::Wait);
    }

    #[test]
    fn no_claims_anywhere_falls_through_past_tiers_3_and_4() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);

        let decision = decide(&conn, &todo);
        assert_ne!(decision.effective_action, EffectiveActionInternal::Wait);
    }

    #[test]
    fn unconfirmed_agent_stays_quiet_not_ask_or_wait() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, 0);
        let mut todo = make_todo(&conn, &pid, &sid, &goal.id);
        agentflare_backend::item::update(
            &conn,
            &todo.id,
            agentflare_backend::item::UpdateItem {
                assignee_agent: Some("opencode".into()),
                ..Default::default()
            },
        )
        .unwrap();
        todo = agentflare_backend::item::get(&conn, &todo.id).unwrap();

        let decision = decide(&conn, &todo);
        assert_eq!(
            decision.effective_action,
            EffectiveActionInternal::StayQuiet
        );
        assert!(!decision.should_run);
    }

    #[test]
    fn confirmed_agent_with_nothing_blocking_runs() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = make_goal_item(&conn, &pid, &sid, GoalLifecycle::Active, 0);
        let todo = make_todo(&conn, &pid, &sid, &goal.id);

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveActionInternal::Run);
        assert!(decision.should_run);
    }

    #[test]
    fn ungrouped_item_with_no_goal_behaves_like_eligibility_only() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let todo = agentflare_backend::item::create(
            &conn,
            agentflare_backend::item::CreateItem {
                project_id: pid.clone(),
                state_id: sid.clone(),
                name: "lone todo".into(),
                description: None,
                priority: None,
                parent_id: None,
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
        .unwrap();

        let decision = decide(&conn, &todo);
        assert_eq!(decision.effective_action, EffectiveActionInternal::Run);
    }

    #[test]
    fn malformed_goal_metadata_fails_closed_to_ask() {
        let conn = test_conn();
        let (pid, sid) = seed_project(&conn);
        let goal = agentflare_backend::item::create(
            &conn,
            agentflare_backend::item::CreateItem {
                project_id: pid.clone(),
                state_id: sid.clone(),
                name: "broken goal".into(),
                description: None,
                priority: None,
                parent_id: None,
                assignee_agent: None,
                sort_order: None,
                external_source: None,
                external_id: None,
                metadata: Some(r#"{"goal":{"objective":"missing required fields"}}"#.into()),
                label_ids: vec![],
                assignee_ids: vec![],
                dependency_ids: vec![],
            },
        )
        .unwrap();
        let todo = make_todo(&conn, &pid, &sid, &goal.id);

        let decision = decide(&conn, &todo);
        assert!(!decision.should_run);
        assert_eq!(decision.effective_action, EffectiveActionInternal::Ask);
        assert!(decision.reason.contains("goal_state_invalid"));
    }
}
