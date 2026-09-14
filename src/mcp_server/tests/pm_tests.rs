use super::*;

#[test]
fn pm_unknown_action_is_invalid_params() {
    let (_tmp, s) = harness();
    let err = s
        .pm(Parameters(PmRequest {
            action: "bogus".into(),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(err.to_string().contains("bogus"));
}

#[test]
fn pm_standup_matches_item_standup_for_the_same_params() {
    let (_tmp, s) = harness();
    let _created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Some work"))).unwrap()).unwrap();

    let via_pm = s
        .pm(Parameters(PmRequest {
            action: "standup".into(),
            cutoff_hours: Some(48),
            ..Default::default()
        }))
        .unwrap();
    let via_item = s
        .item(Parameters(ItemRequest {
            action: "standup".into(),
            cutoff_hours: Some(48),
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(via_pm, via_item);
}

#[test]
fn pm_groom_defaults_state_group_to_backlog_and_unstarted() {
    let (_tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create("Backlog item")))
            .unwrap(),
    )
    .unwrap();

    let groomed: serde_json::Value = serde_json::from_str(
        &s.pm(Parameters(PmRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let shortlist = groomed["items"].as_array().unwrap();
    assert!(
        shortlist.iter().any(|it| it["id"] == created["id"]),
        "expected the fresh backlog item in pm groom's shortlist: {groomed}"
    );
}

#[test]
fn pm_plan_forces_a_capacity_bucket_by_default() {
    let (_tmp, s) = harness();
    let sized: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Sized".into()),
            metadata: Some(serde_json::json!({"size": "S"})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let planned: serde_json::Value = serde_json::from_str(
        &s.pm(Parameters(PmRequest {
            action: "plan".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let now = planned["now"].as_array().unwrap();
    assert!(now.iter().any(|it| *it == sized["id"]), "{planned}");
}

#[test]
fn pm_health_matches_item_health_for_the_same_params() {
    let (_tmp, s) = harness();
    let via_pm = s
        .pm(Parameters(PmRequest {
            action: "health".into(),
            window_weeks: Some(2),
            ..Default::default()
        }))
        .unwrap();
    let via_item = s
        .item(Parameters(ItemRequest {
            action: "health".into(),
            window_weeks: Some(2),
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(via_pm, via_item);
}

#[test]
fn pm_portfolio_rolls_up_every_project_with_a_project_label() {
    let (_tmp, s) = harness();
    let _created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create("Portfolio item")))
            .unwrap(),
    )
    .unwrap();

    let out: serde_json::Value = serde_json::from_str(
        &s.pm(Parameters(PmRequest {
            action: "portfolio".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(out["report"], "health");
    let projects = out["projects"].as_array().unwrap();
    assert!(!projects.is_empty());
    assert!(projects[0]["project"].is_string());
}

#[test]
fn pm_portfolio_rejects_an_unknown_report() {
    let (_tmp, s) = harness();
    let err = s
        .pm(Parameters(PmRequest {
            action: "portfolio".into(),
            report: Some("bogus".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn pm_mode_on_off_status_roundtrip() {
    crate::paths::test_support::with_temp_home(|| {
        let s = AgentflareMcp::default();
        let status: serde_json::Value = serde_json::from_str(
            &s.pm(Parameters(PmRequest {
                action: "mode_status".into(),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(status["active"], false);

        let on: serde_json::Value = serde_json::from_str(
            &s.pm(Parameters(PmRequest {
                action: "mode_on".into(),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(on["active"], true);
        assert!(crate::pm_mode::is_active());

        let off: serde_json::Value = serde_json::from_str(
            &s.pm(Parameters(PmRequest {
                action: "mode_off".into(),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(off["active"], false);
        assert!(!crate::pm_mode::is_active());
    });
}
