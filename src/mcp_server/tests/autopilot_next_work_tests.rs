use super::*;
use serde_json::json;

fn make_item(
    s: &AgentflareMcp,
    name: &str,
    agent: &str,
    label: bool,
    metadata: Option<serde_json::Value>,
) -> String {
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            assignee_agent: Some(agent.into()),
            metadata,
            ..empty_item_create(name)
        }))
        .unwrap(),
    )
    .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    if label {
        let project_id = created["project_id"].as_str().unwrap().to_string();
        s.with_backend_db(|conn| {
            agentflare_backend::label::ensure_project_label(conn, &project_id, "ready-for-work")
        })
        .unwrap()
        .unwrap();
        s.item(Parameters(ItemRequest {
            action: "add_label".into(),
            id: Some(id.clone()),
            label_name: Some("ready-for-work".into()),
            ..Default::default()
        }))
        .unwrap();
    }
    id
}

fn create_ready_item(s: &AgentflareMcp, name: &str, agent: &str) -> String {
    make_item(s, name, agent, true, None)
}

fn next_work(s: &AgentflareMcp, agent: &str) -> serde_json::Value {
    let out = s
        .autopilot_impl(AutopilotRequest {
            action: "next_work".into(),
            agent: Some(agent.into()),
            max_cost_usd: Some(10.0),
            rate_limit_pct: Some(50.0),
            ..Default::default()
        })
        .unwrap();
    serde_json::from_str(&out).unwrap()
}

fn claim_db() -> rusqlite::Connection {
    crate::db::open().unwrap()
}

#[test]
fn next_work_returns_none_ready_when_board_is_empty() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let out = next_work(&s, "claude-code");
        assert_eq!(out["claimed"], false);
        assert_eq!(out["blocked_reason"], "none_ready");
    });
}

#[test]
fn next_work_claims_the_lowest_ready_item_assigned_to_the_agent() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let a = create_ready_item(&s, "A", "claude-code");
        let _b = create_ready_item(&s, "B", "claude-code:some-job-id");
        let out = next_work(&s, "claude-code");
        assert_eq!(out["claimed"], true);
        assert_eq!(out["item"]["id"], a);
        assert_eq!(out["item"]["title"], "A");
        let target = format!("item#{a}");
        assert!(
            crate::claims::list(
                &claim_db(),
                None,
                false,
                crate::claims::now(),
                crate::claims::ttl_secs()
            )
            .unwrap()
            .iter()
            .any(|c| c.target == target)
        );
        // second call: A is ours already, B is next
        let again = next_work(&s, "claude-code");
        assert_eq!(again["claimed"], true);
    });
}

#[test]
fn next_work_skips_items_assigned_elsewhere_and_without_label() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        create_ready_item(&s, "O", "opencode");
        make_item(&s, "N", "claude-code", false, None);
        assert_eq!(next_work(&s, "claude-code")["blocked_reason"], "none_ready");
    });
}

#[test]
fn next_work_skips_items_with_an_open_blocks_dependency() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let blocker = make_item(&s, "blocker", "opencode", false, None);
        let blocked = create_ready_item(&s, "blocked", "claude-code");
        s.item(Parameters(ItemRequest {
            action: "add_relation".into(),
            id: Some(blocked),
            related_item_id: Some(blocker),
            relation_type: Some("blocks".into()),
            ..Default::default()
        }))
        .unwrap();
        assert_eq!(next_work(&s, "claude-code")["blocked_reason"], "none_ready");
    });
}

#[test]
fn next_work_reports_plan_approval_when_the_item_awaits_a_human_plan_approver() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let id = make_item(
            &s,
            "P",
            "claude-code",
            true,
            Some(json!({"plan_required": true, "plan_approver": "human"})),
        );
        let out = next_work(&s, "claude-code");
        assert_eq!(out["claimed"], false);
        assert_eq!(out["blocked_reason"], "plan_approval");
        assert_eq!(out["item"]["id"], id);
    });
}

#[test]
fn next_work_reports_claim_held_when_another_owner_holds_the_item() {
    crate::paths::test_support::with_temp_home(|| {
        let (_tmp, s) = harness();
        let id = create_ready_item(&s, "H", "claude-code");
        crate::claims::acquire(
            &claim_db(),
            &super::super::autopilot_next_work::claim_repo(),
            &format!("item#{id}"),
            "other:1",
            None,
            None,
            crate::claims::now(),
            crate::claims::ttl_secs(),
        )
        .unwrap();
        let out = next_work(&s, "claude-code");
        assert_eq!(out["claimed"], false);
        assert_eq!(out["blocked_reason"], "claim_held");
    });
}

#[test]
fn next_work_rejects_out_of_range_budget_inputs() {
    let (_tmp, s) = harness();
    for (cost, pct) in [(Some(-1.0), None), (None, Some(101.0))] {
        let err = s
            .autopilot_impl(AutopilotRequest {
                action: "next_work".into(),
                agent: Some("claude-code".into()),
                max_cost_usd: cost,
                rate_limit_pct: pct,
                ..Default::default()
            })
            .unwrap_err();
        assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }
}
