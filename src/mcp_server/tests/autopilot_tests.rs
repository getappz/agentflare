use super::*;

#[test]
fn autopilot_unknown_action_is_invalid_params() {
    let (_tmp, s) = harness();
    let err = s
        .autopilot_impl(AutopilotRequest {
            action: "nope".into(),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn autopilot_health_reports_mcp_version_and_daemon_state() {
    let (_tmp, s) = harness();
    let out: serde_json::Value = serde_json::from_str(
        &s.autopilot_impl(AutopilotRequest {
            action: "health".into(),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(out["mcp_version"], env!("CARGO_PKG_VERSION"));
    assert!(["up", "down", "unknown"].contains(&out["daemon"].as_str().unwrap()));
    assert!(out["stale_binary"].is_boolean());
}

#[test]
fn autopilot_is_listed_as_a_builtin_tool() {
    assert!(
        builtin_tools::search_builtin_tools("autopilot", 5, gateway_registry::MatchMode::Any)
            .iter()
            .any(|h| h.tool == "mcp__flare__autopilot")
    );
}
