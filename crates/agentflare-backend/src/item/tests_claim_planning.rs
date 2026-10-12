use super::*;

fn gated(conn: &Connection, pid: &str, sid: &str, metadata: &str, labeled: bool) -> Item {
    let label_ids = if labeled {
        vec![
            crate::label::ensure_project_label(conn, pid, NEEDS_PLAN_LABEL)
                .unwrap()
                .id,
        ]
    } else {
        vec![]
    };
    create(
        conn,
        CreateItem {
            project_id: pid.to_string(),
            state_id: sid.to_string(),
            name: "epic".into(),
            description: None,
            priority: None,
            parent_id: None,
            assignee_agent: None,
            sort_order: None,
            external_source: None,
            external_id: None,
            metadata: Some(metadata.to_string()),
            label_ids,
            assignee_ids: vec![],
            dependency_ids: vec![],
            start_date: None,
            due_date: None,
        },
    )
    .unwrap()
}

fn blocked(status: &str) -> ClaimOutcome {
    ClaimOutcome::BlockedByPlan {
        status: status.into(),
    }
}

#[test]
fn planning_claim_acquires_a_gated_needs_plan_item_without_opening_the_gate() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "_pc1");
    let item = gated(&conn, &pid, &sid, r#"{"plan_required":true}"#, true);

    let out = claim_for_planning(&conn, &item.id, "planner:1", now(), 3600).unwrap();
    assert_eq!(out, ClaimOutcome::Acquired);
    let after = get(&conn, &item.id).unwrap();
    assert_eq!(after.assignee_agent.as_deref(), Some("planner:1"));
    assert!(after.started_at.is_some());

    // The gate is untouched: nobody can implement it yet.
    assert_eq!(
        claim(&conn, &item.id, "claude-code:1", now(), 3600).unwrap(),
        blocked("none")
    );
}

#[test]
fn a_second_planner_is_held_off_by_the_lease() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "_pc2");
    let item = gated(&conn, &pid, &sid, r#"{"plan_required":true}"#, true);
    assert_eq!(
        claim_for_planning(&conn, &item.id, "planner:1", now(), 3600).unwrap(),
        ClaimOutcome::Acquired
    );
    let out = claim_for_planning(&conn, &item.id, "planner:2", now(), 3600).unwrap();
    assert!(matches!(out, ClaimOutcome::Held { .. }), "{out:?}");
}

#[test]
fn rejected_plans_may_be_replanned_but_pending_and_approved_may_not() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "_pc3");
    let rejected = gated(
        &conn,
        &pid,
        &sid,
        r#"{"plan_required":true,"plan_status":"rejected"}"#,
        true,
    );
    let pending = gated(
        &conn,
        &pid,
        &sid,
        r#"{"plan_required":true,"plan_status":"pending"}"#,
        true,
    );
    let approved = gated(
        &conn,
        &pid,
        &sid,
        r#"{"plan_required":true,"plan_status":"approved"}"#,
        true,
    );

    assert_eq!(
        claim_for_planning(&conn, &rejected.id, "planner:1", now(), 3600).unwrap(),
        ClaimOutcome::Acquired
    );
    assert_eq!(
        claim_for_planning(&conn, &pending.id, "planner:1", now(), 3600).unwrap(),
        blocked("pending")
    );
    assert_eq!(
        claim_for_planning(&conn, &approved.id, "planner:1", now(), 3600).unwrap(),
        blocked("approved")
    );
}

#[test]
fn missing_label_or_missing_gate_refuses_instead_of_erroring() {
    let conn = db::open_in_memory().unwrap();
    let (pid, sid) = seed_project(&conn, "_pc4");
    // Project has no `needs-plan` label at all yet.
    let unlabeled = gated(&conn, &pid, &sid, r#"{"plan_required":true}"#, false);
    assert_eq!(
        claim_for_planning(&conn, &unlabeled.id, "planner:1", now(), 3600).unwrap(),
        blocked("no_needs_plan_label")
    );
    // The label now exists in the project, but this item lacks it.
    let _labeled = gated(&conn, &pid, &sid, r#"{"plan_required":true}"#, true);
    let other = gated(&conn, &pid, &sid, r#"{"plan_required":true}"#, false);
    assert_eq!(
        claim_for_planning(&conn, &other.id, "planner:1", now(), 3600).unwrap(),
        blocked("no_needs_plan_label")
    );
    // No gate at all: planning is meaningless.
    let ungated = gated(&conn, &pid, &sid, "{}", true);
    assert_eq!(
        claim_for_planning(&conn, &ungated.id, "planner:1", now(), 3600).unwrap(),
        blocked("not_required")
    );
}
