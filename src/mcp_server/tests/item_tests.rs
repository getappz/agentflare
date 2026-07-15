use super::*;

#[test]
fn item_create_auto_provisions_workspace_and_project() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test Item"))).unwrap()).unwrap();
    assert_eq!(created["name"], "Test Item");
    assert_eq!(created["sequence_id"], 1);
    assert!(created["project_id"].as_str().is_some());
}

#[test]
fn item_create_rejects_empty_name() {
    let (_tmp, s) = harness();
    let err = s.item(Parameters(empty_item_create(""))).unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn item_update_state_sets_timestamps_via_mcp() {
    let (tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let project_id = created["project_id"].as_str().unwrap().to_string();

    let started_state_id = {
        let conn = backend_conn(&tmp);
        agentflare_backend::state::list_by_project(&conn, &project_id)
            .unwrap()
            .into_iter()
            .find(|st| st.group_name == "started")
            .unwrap()
            .id
    };

    let updated: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(item_id),
            state_id: Some(started_state_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(updated["started_at"].is_number());
    assert!(updated["completed_at"].is_null());
}

#[test]
fn item_cancel_moves_to_cancelled_state() {
    let (tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let project_id = created["project_id"].as_str().unwrap().to_string();

    let cancelled: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "cancel".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let state_id = cancelled["state_id"].as_str().unwrap().to_string();

    let conn = backend_conn(&tmp);
    let group = agentflare_backend::state::list_by_project(&conn, &project_id)
        .unwrap()
        .into_iter()
        .find(|st| st.id == state_id)
        .unwrap()
        .group_name;
    assert_eq!(group, "cancelled");
}

#[test]
fn item_cancel_releases_the_callers_own_claim() {
    // `claim` always resolves a worktree_repo_root and may run real `git
    // worktree` commands against it — every test that calls `claim` must
    // override this to an isolated throwaway repo, never the repo
    // `cargo test` itself is running in. Same scaffolding as
    // `item_claim_response_includes_worktree_path`.
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().to_path_buf();
    let run_git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&repo_root)
            .output()
            .unwrap()
    };
    run_git(&["init", "-b", "master"]);
    run_git(&["config", "user.email", "test@test.com"]);
    run_git(&["config", "user.name", "Test"]);
    run_git(&["commit", "--allow-empty", "-m", "initial"]);

    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root),
        ..Default::default()
    };

    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    s.item(Parameters(ItemRequest {
        action: "claim".into(),
        id: Some(item_id.clone()),
        ..Default::default()
    }))
    .unwrap();

    s.item(Parameters(ItemRequest {
        action: "cancel".into(),
        id: Some(item_id.clone()),
        ..Default::default()
    }))
    .unwrap();

    // The claim must be released — re-claiming should succeed
    // immediately instead of coming back "held".
    let reclaimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(reclaimed["status"], "acquired");
}

fn claim_harness() -> (AgentflareMcp, tempfile::TempDir) {
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().to_path_buf();
    let run_git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&repo_root)
            .output()
            .unwrap()
    };
    run_git(&["init", "-b", "master"]);
    run_git(&["config", "user.email", "test@test.com"]);
    run_git(&["config", "user.name", "Test"]);
    run_git(&["commit", "--allow-empty", "-m", "initial"]);
    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root),
        ..Default::default()
    };
    (s, tmp)
}

#[test]
fn item_update_assignee_to_different_agent_releases_old_claim() {
    let (s, _tmp) = claim_harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let claimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(claimed["status"], "acquired");

    // Reassign to a different agent — should release the old claim.
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(item_id.clone()),
        assignee_agent: Some("claude-code".into()),
        ..Default::default()
    }))
    .unwrap();

    // The claim row must be gone — the only thing that proves a release happened.
    // (Re-claiming by the same owner returns "acquired" either way.)
    assert_eq!(
        s.with_backend_db(|conn| agentflare_backend::claim::current_owner(conn, &item_id))
            .unwrap(),
        None
    );
}

#[test]
fn item_update_assignee_to_different_instance_does_not_release_claim() {
    let (s, _tmp) = claim_harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let claimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(claimed["status"], "acquired");
    let owner = claimed["owner"].as_str().unwrap().to_string();

    // Detect the agent portion of the owner (e.g. "opencode" from "opencode:13112")
    // and reassign to the same agent with a different instance — claim stays held.
    let my_agent = crate::claims::agent_of(&owner);
    let same_agent_different_instance = format!("{my_agent}:99999");

    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(item_id.clone()),
        assignee_agent: Some(same_agent_different_instance),
        ..Default::default()
    }))
    .unwrap();

    // Verify claim still held by the original owner via backend query.
    assert_eq!(
        s.with_backend_db(|conn| agentflare_backend::claim::current_owner(conn, &item_id))
            .unwrap(),
        Some(owner)
    );
}

#[test]
fn item_list_rejects_negative_limit_and_offset() {
    let (_tmp, s) = harness();
    let err = s
        .item(Parameters(ItemRequest {
            action: "list".into(),
            limit: Some(-1),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

    let err = s
        .item(Parameters(ItemRequest {
            action: "list".into(),
            offset: Some(-1),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn item_list_filters_by_assignee_or_unassigned_and_sorts_open_first() {
    let (tmp, s) = harness();
    let mine_open: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Mine open"))).unwrap()).unwrap();
    let project_id = mine_open["project_id"].as_str().unwrap().to_string();
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(mine_open["id"].as_str().unwrap().to_string()),
        assignee_agent: Some("me".into()),
        ..Default::default()
    }))
    .unwrap();

    serde_json::from_str::<serde_json::Value>(
        &s.item(Parameters(empty_item_create("Unassigned"))).unwrap(),
    )
    .unwrap();

    let others: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Others"))).unwrap()).unwrap();
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(others["id"].as_str().unwrap().to_string()),
        assignee_agent: Some("someone-else".into()),
        ..Default::default()
    }))
    .unwrap();

    let mine_done: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Mine done"))).unwrap()).unwrap();
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(mine_done["id"].as_str().unwrap().to_string()),
        assignee_agent: Some("me".into()),
        ..Default::default()
    }))
    .unwrap();
    let done_state_id = {
        let conn = backend_conn(&tmp);
        agentflare_backend::state::list_by_project(&conn, &project_id)
            .unwrap()
            .into_iter()
            .find(|st| st.group_name == "completed")
            .unwrap()
            .id
    };
    s.item(Parameters(ItemRequest {
        action: "update_state".into(),
        id: Some(mine_done["id"].as_str().unwrap().to_string()),
        state_id: Some(done_state_id),
        ..Default::default()
    }))
    .unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            assignee_agent: Some("me".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let names: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Mine open", "Unassigned", "Mine done"]);
}

#[test]
fn item_list_defaults_assignee_filter_to_server_identity() {
    // #75: a bare `item(list)` (no assignee_agent) must default to the
    // server-derived identity — mine + unassigned — not dump every item.
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        agent: Some("me".into()),
        ..Default::default()
    };

    let mine: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Mine"))).unwrap()).unwrap();
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(mine["id"].as_str().unwrap().to_string()),
        assignee_agent: Some("me".into()),
        ..Default::default()
    }))
    .unwrap();

    serde_json::from_str::<serde_json::Value>(
        &s.item(Parameters(empty_item_create("Unassigned"))).unwrap(),
    )
    .unwrap();

    let others: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Others"))).unwrap()).unwrap();
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(others["id"].as_str().unwrap().to_string()),
        assignee_agent: Some("someone-else".into()),
        ..Default::default()
    }))
    .unwrap();

    // Bare list: no assignee_agent → defaults to "me" (mine + unassigned).
    let defaulted: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let mut names: Vec<&str> = defaulted
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, vec!["Mine", "Unassigned"]);

    // An explicit assignee_agent is still honored (view a teammate's queue).
    let explicit: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            assignee_agent: Some("someone-else".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let mut names2: Vec<&str> = explicit
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    names2.sort_unstable();
    assert_eq!(names2, vec!["Others", "Unassigned"]);
}

#[test]
fn item_list_state_group_filter_accepts_comma_separated_groups() {
    let (tmp, s) = harness();
    let open_item: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Open"))).unwrap()).unwrap();
    let project_id = open_item["project_id"].as_str().unwrap().to_string();
    let done_item: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Done"))).unwrap()).unwrap();
    let cancelled_item: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Cancelled"))).unwrap()).unwrap();

    let conn = backend_conn(&tmp);
    let states = agentflare_backend::state::list_by_project(&conn, &project_id).unwrap();
    let done_state_id = states
        .iter()
        .find(|st| st.group_name == "completed")
        .unwrap()
        .id
        .clone();
    let cancelled_state_id = states
        .iter()
        .find(|st| st.group_name == "cancelled")
        .unwrap()
        .id
        .clone();
    drop(conn);

    s.item(Parameters(ItemRequest {
        action: "update_state".into(),
        id: Some(done_item["id"].as_str().unwrap().to_string()),
        state_id: Some(done_state_id),
        ..Default::default()
    }))
    .unwrap();
    s.item(Parameters(ItemRequest {
        action: "update_state".into(),
        id: Some(cancelled_item["id"].as_str().unwrap().to_string()),
        state_id: Some(cancelled_state_id),
        ..Default::default()
    }))
    .unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            state_group: Some("backlog,completed".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let names: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["Open", "Done"]);
}

#[test]
fn item_groom_flags_unassigned_and_computes_pull_next() {
    let (_tmp, s) = harness();
    let foo: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Foo"))).unwrap()).unwrap();
    s.item(Parameters(ItemRequest {
        action: "create".into(),
        name: Some("Bar".into()),
        assignee_agent: Some("someone".into()),
        ..Default::default()
    }))
    .unwrap();

    let groomed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let items = groomed["items"].as_array().unwrap();
    let foo_entry = items
        .iter()
        .find(|i| i["name"] == "Foo")
        .expect("Foo present");
    assert_eq!(foo_entry["unassigned"], true);
    assert_eq!(foo_entry["stale"], false);
    let bar_entry = items
        .iter()
        .find(|i| i["name"] == "Bar")
        .expect("Bar present");
    assert_eq!(bar_entry["unassigned"], false);

    let pull_next: Vec<&str> = groomed["pull_next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(pull_next.contains(&foo["id"].as_str().unwrap()));
    assert_eq!(groomed["unassigned_count"], 1);
}

/// Regression (CodeRabbit): a completed dependency must never read back
/// as an open blocker just because it fell outside the shortlist's
/// default state_group filter (completed items aren't in
/// "backlog,unstarted", so the naive shortlist-scoped lookup used to
/// return "" for its state and treat that as "still open").
#[test]
fn item_groom_does_not_block_on_a_completed_dependency_outside_the_shortlist() {
    let (_tmp, s) = harness();
    let dep: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Dep"))).unwrap()).unwrap();
    let project_id = dep["project_id"].as_str().unwrap().to_string();
    let blocked: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Blocked".into()),
            dependency_ids: Some(vec![dep["id"].as_str().unwrap().to_string()]),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let conn = backend_conn(&_tmp);
    let completed_state = agentflare_backend::state::list_by_project(&conn, &project_id)
        .unwrap()
        .into_iter()
        .find(|st| st.group_name == "completed")
        .unwrap()
        .id;
    drop(conn);
    s.item(Parameters(ItemRequest {
        action: "update_state".into(),
        id: Some(dep["id"].as_str().unwrap().to_string()),
        state_id: Some(completed_state),
        ..Default::default()
    }))
    .unwrap();

    // Default state_group is "backlog,unstarted" — Dep (now completed)
    // falls outside the shortlist entirely.
    let groomed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let items = groomed["items"].as_array().unwrap();
    assert!(
        !items.iter().any(|i| i["id"] == dep["id"]),
        "completed Dep should not be in the default shortlist"
    );
    let blocked_entry = items.iter().find(|i| i["id"] == blocked["id"]).unwrap();
    assert_eq!(
        blocked_entry["blocked_by"].as_array().unwrap().len(),
        0,
        "a completed dependency must not block, even when it's outside the shortlist"
    );
}

/// Regression (CodeRabbit): fan-in must count dependents project-wide,
/// not just other items that happen to share the same shortlist.
#[test]
fn item_groom_fanin_counts_dependents_outside_the_shortlist() {
    let (_tmp, s) = harness();
    let target: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Target"))).unwrap()).unwrap();
    let project_id = target["project_id"].as_str().unwrap().to_string();
    let dependent: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Dependent".into()),
            dependency_ids: Some(vec![target["id"].as_str().unwrap().to_string()]),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let conn = backend_conn(&_tmp);
    let completed_state = agentflare_backend::state::list_by_project(&conn, &project_id)
        .unwrap()
        .into_iter()
        .find(|st| st.group_name == "completed")
        .unwrap()
        .id;
    drop(conn);
    // Move the dependent out of the default shortlist filter — Target's
    // fan-in must still count it.
    s.item(Parameters(ItemRequest {
        action: "update_state".into(),
        id: Some(dependent["id"].as_str().unwrap().to_string()),
        state_id: Some(completed_state),
        ..Default::default()
    }))
    .unwrap();

    let groomed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let items = groomed["items"].as_array().unwrap();
    assert!(!items.iter().any(|i| i["id"] == dependent["id"]));
    let target_entry = items.iter().find(|i| i["id"] == target["id"]).unwrap();
    assert_eq!(target_entry["depended_on_by_count"], 1);
}

#[test]
fn item_groom_flags_blocked_by_open_dependency() {
    let (_tmp, s) = harness();
    let dep: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Dep"))).unwrap()).unwrap();
    let dep_id = dep["id"].as_str().unwrap().to_string();
    let blocked: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Blocked".into()),
            dependency_ids: Some(vec![dep_id.clone()]),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let groomed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let items = groomed["items"].as_array().unwrap();
    let blocked_entry = items
        .iter()
        .find(|i| i["id"] == blocked["id"])
        .expect("Blocked present");
    let blocked_by: Vec<&str> = blocked_entry["blocked_by"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(blocked_by, vec![dep_id.as_str()]);

    let dep_entry = items.iter().find(|i| i["id"] == dep["id"]).unwrap();
    assert_eq!(dep_entry["depended_on_by_count"], 1);

    let pull_next: Vec<&str> = groomed["pull_next"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(!pull_next.contains(&blocked["id"].as_str().unwrap()));
}

#[test]
fn item_groom_detects_near_duplicate_names() {
    let (_tmp, s) = harness();
    let a: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create(
            "FIX-08 backlog low unassigned stale",
        )))
        .unwrap(),
    )
    .unwrap();
    let b: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create(
            "FIX-09 backlog low unassigned stale duplicateish",
        )))
        .unwrap(),
    )
    .unwrap();

    let groomed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let items = groomed["items"].as_array().unwrap();
    let a_entry = items.iter().find(|i| i["id"] == a["id"]).unwrap();
    let dups: Vec<&str> = a_entry["possible_duplicates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(dups.contains(&b["id"].as_str().unwrap()));
}

#[test]
fn item_update_sets_metadata() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Sized"))).unwrap()).unwrap();
    let updated: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "update".into(),
            id: Some(created["id"].as_str().unwrap().to_string()),
            metadata: Some(serde_json::json!({"size": "M"})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        updated["metadata"],
        serde_json::json!({"size": "M"}).to_string()
    );
}

#[test]
fn item_groom_reads_size_and_flags_unestimated() {
    let (_tmp, s) = harness();
    let sized: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Sized".into()),
            metadata: Some(serde_json::json!({"size": "L"})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let bare: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Bare"))).unwrap()).unwrap();

    let groomed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let items = groomed["items"].as_array().unwrap();
    let sized_entry = items.iter().find(|i| i["id"] == sized["id"]).unwrap();
    assert_eq!(sized_entry["size"], "L");
    assert_eq!(sized_entry["unestimated"], false);
    let bare_entry = items.iter().find(|i| i["id"] == bare["id"]).unwrap();
    assert_eq!(bare_entry["size"], serde_json::Value::Null);
    assert_eq!(bare_entry["unestimated"], true);
    assert_eq!(groomed["unestimated_count"], 1);
}

/// Regression: some callers double-encode an object-typed `metadata` param
/// as a JSON string containing JSON — reproduced live via item(create)
/// with metadata={"size":"S"}, which stored `"{\"size\": \"S\"}"` (a
/// string) rather than the object itself. `groom` must still read `size`
/// through that extra layer instead of silently reporting `unestimated`.
#[test]
fn item_groom_reads_size_through_double_encoded_metadata() {
    let (_tmp, s) = harness();
    let double_encoded: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Double-encoded".into()),
            metadata: Some(serde_json::Value::String(
                serde_json::json!({"size": "M"}).to_string(),
            )),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let groomed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let entry = groomed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == double_encoded["id"])
        .unwrap();
    assert_eq!(entry["size"], "M");
    assert_eq!(entry["unestimated"], false);
}

#[test]
fn item_groom_capacity_buckets_now_next_later_and_needs_estimation() {
    let (_tmp, s) = harness();
    let sized = |name: &str, size: &str| ItemRequest {
        action: "create".into(),
        name: Some(name.into()),
        metadata: Some(serde_json::json!({"size": size})),
        ..Default::default()
    };
    let ready_a: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(sized("Ready A", "S"))).unwrap()).unwrap();
    let ready_b: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(sized("Ready B", "S"))).unwrap()).unwrap();
    let dep: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Dep"))).unwrap()).unwrap();
    let blocked: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            dependency_ids: Some(vec![dep["id"].as_str().unwrap().to_string()]),
            ..sized("Blocked", "M")
        }))
        .unwrap(),
    )
    .unwrap();
    let unestimated: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Unsized"))).unwrap()).unwrap();

    // No capacity: buckets omitted entirely (backward compatible).
    let unbucketed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(unbucketed.get("now").is_none());

    let groomed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            capacity: Some(1),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let ids = |key: &str| -> Vec<String> {
        groomed[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    let now = ids("now");
    let next = ids("next");
    assert_eq!(now.len(), 1, "capacity=1 caps now to 1 ready item");
    assert!(
        now.contains(&ready_a["id"].as_str().unwrap().to_string())
            || now.contains(&ready_b["id"].as_str().unwrap().to_string())
    );
    // Whichever ready item didn't make `now` spills into `next`.
    assert_eq!(now.len() + next.len(), 2);
    assert_eq!(ids("later"), vec![blocked["id"].as_str().unwrap()]);
    // "Dep" has no size either — unestimated, same as the dedicated "Unsized" item.
    let mut needs_est = ids("needs_estimation");
    needs_est.sort_unstable();
    let mut expected = vec![
        dep["id"].as_str().unwrap().to_string(),
        unestimated["id"].as_str().unwrap().to_string(),
    ];
    expected.sort_unstable();
    assert_eq!(needs_est, expected);
}

/// Regression (CodeRabbit): standup's "done" filter and health's
/// velocity bucketing must key off `completed_at`, not `updated_at` —
/// editing an already-completed item (e.g. fixing a typo) bumps
/// `updated_at` without re-completing it, and must not make old work
/// spuriously reappear as "just done" or shift which week it counts in.
#[test]
fn item_standup_and_health_use_completed_at_not_updated_at() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Old work"))).unwrap()).unwrap();
    let project_id = created["project_id"].as_str().unwrap().to_string();
    let id = created["id"].as_str().unwrap().to_string();
    let conn = backend_conn(&_tmp);
    let completed_state = agentflare_backend::state::list_by_project(&conn, &project_id)
        .unwrap()
        .into_iter()
        .find(|st| st.group_name == "completed")
        .unwrap()
        .id;
    drop(conn);
    s.item(Parameters(ItemRequest {
        action: "update_state".into(),
        id: Some(id.clone()),
        state_id: Some(completed_state),
        ..Default::default()
    }))
    .unwrap();

    // Simulate: completed long ago, then edited just now (updated_at
    // recent, completed_at old) — direct SQL, no clock control in tests.
    let old_ts = 1_700_000_000_i64; // long before "now" in this fixture era
    let conn = backend_conn(&_tmp);
    conn.execute(
        "UPDATE items SET completed_at = ?1 WHERE id = ?2",
        rusqlite::params![old_ts, id],
    )
    .unwrap();
    drop(conn);
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(id.clone()),
        description: Some("fixed a typo".into()),
        ..Default::default()
    }))
    .unwrap();

    let standup: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "standup".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        !standup["done"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == id),
        "editing an old completed item must not resurrect it in 'done'"
    );

    let health: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "health".into(),
            window_weeks: Some(1),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        health["velocity"][0]["completed_count"], 0,
        "an old completion must not count in this week's velocity just because it was edited"
    );
}

#[test]
fn item_standup_buckets_done_in_progress_grouped_and_stuck() {
    let (_tmp, s) = harness();
    let project_id: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("bootstrap"))).unwrap()).unwrap();
    let project_id = project_id["project_id"].as_str().unwrap().to_string();
    let conn = backend_conn(&_tmp);
    let states = agentflare_backend::state::list_by_project(&conn, &project_id).unwrap();
    let started_state = states
        .iter()
        .find(|st| st.group_name == "started")
        .unwrap()
        .id
        .clone();
    let completed_state = states
        .iter()
        .find(|st| st.group_name == "completed")
        .unwrap()
        .id
        .clone();
    drop(conn);

    let move_to = |name: &str, assignee: Option<&str>, state_id: &str| -> serde_json::Value {
        let created: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "create".into(),
                name: Some(name.into()),
                assignee_agent: assignee.map(String::from),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        s.item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(created["id"].as_str().unwrap().to_string()),
            state_id: Some(state_id.to_string()),
            ..Default::default()
        }))
        .unwrap();
        created
    };

    let wip_alice = move_to("WIP Alice", Some("alice"), &started_state);
    let _wip_bob = move_to("WIP Bob", Some("bob"), &started_state);
    let _wip_unassigned = move_to("WIP Unassigned", None, &started_state);
    let done_item = move_to("Done item", Some("alice"), &completed_state);

    let standup: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "standup".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(standup["done_count"], 1);
    assert_eq!(standup["done"][0]["id"], done_item["id"]);
    assert_eq!(standup["in_progress_count"], 3);
    let groups: Vec<&str> = standup["in_progress"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["assignee"].as_str().unwrap())
        .collect();
    assert_eq!(groups, vec!["alice", "bob", "unassigned"]);
    let alice_group = standup["in_progress"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["assignee"] == "alice")
        .unwrap();
    assert_eq!(alice_group["items"][0]["id"], wip_alice["id"]);
    // Nothing is 7+ days old in a freshly-created fixture.
    assert_eq!(standup["stuck_count"], 0);
}

#[test]
fn item_health_reports_velocity_wip_and_bottleneck_placeholder() {
    let (_tmp, s) = harness();
    let project_id: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("bootstrap"))).unwrap()).unwrap();
    let project_id = project_id["project_id"].as_str().unwrap().to_string();
    let conn = backend_conn(&_tmp);
    let states = agentflare_backend::state::list_by_project(&conn, &project_id).unwrap();
    let started_state = states
        .iter()
        .find(|st| st.group_name == "started")
        .unwrap()
        .id
        .clone();
    let completed_state = states
        .iter()
        .find(|st| st.group_name == "completed")
        .unwrap()
        .id
        .clone();
    drop(conn);

    let move_to = |name: &str, state_id: &str| {
        let created: serde_json::Value =
            serde_json::from_str(&s.item(Parameters(empty_item_create(name))).unwrap()).unwrap();
        s.item(Parameters(ItemRequest {
            action: "update_state".into(),
            id: Some(created["id"].as_str().unwrap().to_string()),
            state_id: Some(state_id.to_string()),
            ..Default::default()
        }))
        .unwrap();
    };
    move_to("Done 1", &completed_state);
    move_to("Done 2", &completed_state);
    move_to("WIP", &started_state);

    let health: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "health".into(),
            window_weeks: Some(2),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    let velocity = health["velocity"].as_array().unwrap();
    assert_eq!(velocity.len(), 2, "oldest -> newest, 2 requested windows");
    assert_eq!(
        velocity[1]["completed_count"], 2,
        "current week has both Done items"
    );
    assert_eq!(velocity[0]["completed_count"], 0, "prior week is empty");
    assert_eq!(health["velocity_trend"], "up");
    assert_eq!(health["wip_count"], 1);
    assert_eq!(health["stuck_count"], 0);
    assert_eq!(health["bottlenecks"].as_array().unwrap().len(), 0);
    assert!(
        health["bottleneck_note"]
            .as_str()
            .unwrap()
            .contains("no handoff history")
    );
}

/// Regression (CodeRabbit): an absurd `window_weeks` must be clamped,
/// not used to size a `Vec<VelocityWeek>` directly — otherwise a caller
/// passing e.g. `i64::MAX` drives a near-infinite allocation while the
/// backend DB lock is held.
#[test]
fn item_health_clamps_window_weeks_to_a_sane_maximum() {
    let (_tmp, s) = harness();
    let health: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "health".into(),
            window_weeks: Some(i64::MAX),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(health["window_weeks"], 52);
    assert_eq!(health["velocity"].as_array().unwrap().len(), 52);
}

/// Regression (CodeRabbit): an absurd groom `limit` must be clamped —
/// bounds the O(n^2) duplicate-detection pass and the SQLite `IN (...)`
/// parameter list built from the shortlist.
#[test]
fn item_groom_clamps_limit_to_a_sane_maximum() {
    let (_tmp, s) = harness();
    let groomed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "groom".into(),
            limit: Some(i64::MAX),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(groomed["items"].as_array().unwrap().len() <= 200);
}

/// Real measured comparison, not an estimate: one `groom` call vs. the
/// `list` + N×`get` path it replaces, against a backlog-sized dataset (60
/// items — close to this project's real ~40-item backlog) with dependency
/// edges so `groom`'s blocked/fan-in computation does real work too. Not a
/// hard perf gate (`#[ignore]`, run explicitly) — timing assertions in CI
/// are flaky; this is for a human to re-run and read the numbers.
#[test]
#[ignore = "manual benchmark — run with: cargo test item_groom_benchmark -- --ignored --nocapture"]
fn item_groom_benchmark() {
    let (_tmp, s) = harness();
    let mut ids: Vec<String> = Vec::with_capacity(60);
    for n in 0..60 {
        let priority = ["urgent", "high", "medium", "low", "none"][n % 5];
        let created: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "create".into(),
                name: Some(format!("Benchmark item {n}")),
                description: Some(
                    "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ".repeat(20),
                ),
                priority: Some(priority.into()),
                dependency_ids: if n > 0 && n % 7 == 0 {
                    Some(vec![ids[n - 1].clone()])
                } else {
                    None
                },
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        ids.push(created["id"].as_str().unwrap().to_string());
    }

    let groom_start = std::time::Instant::now();
    let groomed = s
        .item(Parameters(ItemRequest {
            action: "groom".into(),
            ..Default::default()
        }))
        .unwrap();
    let groom_elapsed = groom_start.elapsed();

    let old_start = std::time::Instant::now();
    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            state_group: Some("backlog,unstarted".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let shortlist_ids: Vec<String> = listed
        .as_array()
        .unwrap()
        .iter()
        .take(15)
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    for id in &shortlist_ids {
        s.item(Parameters(ItemRequest {
            action: "get".into(),
            id: Some(id.clone()),
            ..Default::default()
        }))
        .unwrap();
    }
    let old_elapsed = old_start.elapsed();

    println!(
        "groom (1 call): {groom_elapsed:?} | list+{}xget (old path): {old_elapsed:?} | speedup: {:.1}x",
        shortlist_ids.len(),
        old_elapsed.as_secs_f64() / groom_elapsed.as_secs_f64().max(1e-9)
    );
    assert!(groomed.contains("pull_next"));
}

#[test]
fn item_list_respects_limit_and_offset() {
    let (_tmp, s) = harness();
    for name in ["A", "B", "C"] {
        s.item(Parameters(empty_item_create(name))).unwrap();
    }
    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            limit: Some(1),
            offset: Some(1),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let names: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["B"]);
}

#[test]
fn item_list_returns_lean_projection_with_readable_state() {
    let (_tmp, s) = harness();
    s.item(Parameters(empty_item_create("Test"))).unwrap();
    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let first = &listed.as_array().unwrap()[0];
    assert_eq!(first["state"], "Backlog");
    assert_eq!(first["state_group"], "backlog");
    assert!(first.get("description").is_none());
    assert!(first.get("metadata").is_none());
}

#[test]
fn resolve_workspace_id_creates_once_and_reuses() {
    let (tmp, _s) = harness();
    let conn = backend_conn(&tmp);
    let id1 = AgentflareMcp::resolve_workspace_id(&conn).unwrap();
    let id2 = AgentflareMcp::resolve_workspace_id(&conn).unwrap();
    assert_eq!(id1, id2);
}

/// If `.agentflare/project.json` is deleted (wiped worktree, `rm -rf`,
/// etc.) while the project it pointed to still exists, resolving again
/// must reconnect to that same project — not silently fork a duplicate,
/// which would strand the original project's items.
#[test]
fn resolve_project_relinks_to_existing_project_when_link_file_is_deleted() {
    let (tmp, s) = harness();
    let conn = backend_conn(&tmp);
    let first = s.resolve_project(&conn).unwrap();

    std::fs::remove_file(s.project_link_path()).unwrap();

    let second = s.resolve_project(&conn).unwrap();
    assert_eq!(
        first.id, second.id,
        "must reconnect to the same project, not fork a duplicate"
    );
    let all = agentflare_backend::project::list_by_workspace(&conn, &first.workspace_id).unwrap();
    assert_eq!(
        all.len(),
        1,
        "no duplicate project should have been created: {all:?}"
    );
}

/// Two different repos can easily share a directory basename (or, for
/// non-git dirs, no distinguishing info at all beyond the name). They
/// must never be conflated into one project just because they'd derive
/// the same display identifier — each gets its own project, with the
/// second disambiguated by a suffix.
#[test]
fn resolve_project_does_not_conflate_different_repos_with_the_same_derived_name() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("backend.db");
    let s1 = AgentflareMcp {
        backend_db_override: Some(db_path.clone()),
        backend_project_link_override: Some(tmp.path().join("link1.json")),
        backend_repo_key_override: Some("path:/repo/one".to_string()),
        ..Default::default()
    };
    let s2 = AgentflareMcp {
        backend_db_override: Some(db_path.clone()),
        backend_project_link_override: Some(tmp.path().join("link2.json")),
        backend_repo_key_override: Some("path:/repo/two".to_string()),
        ..Default::default()
    };
    let conn = agentflare_backend::db::open_db(&db_path).unwrap();
    let p1 = s1.resolve_project(&conn).unwrap();
    let p2 = s2.resolve_project(&conn).unwrap();
    assert_ne!(
        p1.id, p2.id,
        "different repos must never share a project even with the same derived name"
    );
    assert_ne!(
        p1.identifier, p2.identifier,
        "the second project must get a disambiguating suffix"
    );

    // Each keeps resolving to its own project on repeat calls.
    assert_eq!(s1.resolve_project(&conn).unwrap().id, p1.id);
    assert_eq!(s2.resolve_project(&conn).unwrap().id, p2.id);
}

/// Non-git projects need the same "root is stable no matter which
/// subdirectory you're in" guarantee git repos get for free from `git
/// rev-parse --show-toplevel` — otherwise the same project would split
/// across multiple `.agentflare/project.json` files depending on which
/// subdirectory a tool happened to be called from.
#[test]
fn find_root_from_walks_up_to_the_nearest_marker() {
    // Bounding "home" at the tempdir's own parent contains the walk
    // entirely within this test's constructed tree — passing some
    // unrelated path here would NOT do that: the walk follows the real
    // filesystem's `.parent()` chain regardless, so it would keep
    // climbing past `root` into real ancestor directories (which may
    // have their own real markers, e.g. this machine's actual
    // `~/.agentflare`) until it happened to reach that unrelated path,
    // which — not being a real ancestor — it never would, walking all
    // the way to the filesystem root instead.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.parent().unwrap();
    std::fs::write(root.join("package.json"), "{}").unwrap();
    let deep = root.join("src").join("nested").join("deep");
    std::fs::create_dir_all(&deep).unwrap();

    assert_eq!(AgentflareMcp::find_root_from(&deep, home), root);
    assert_eq!(AgentflareMcp::find_root_from(root, home), root);
}

#[test]
fn find_root_from_prefers_an_existing_agentflare_link_over_other_markers() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let home = root.parent().unwrap();
    // A nested directory with its own marker (e.g. a sub-package) must
    // not shadow an ancestor's existing project link — the
    // .agentflare pass runs before the ROOT_MARKERS pass for
    // exactly this reason.
    std::fs::create_dir_all(root.join(".agentflare")).unwrap();
    let sub = root.join("packages").join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("package.json"), "{}").unwrap();

    assert_eq!(AgentflareMcp::find_root_from(&sub, home), root);
    assert_eq!(AgentflareMcp::find_root_from(root, home), root);
}

/// The boundary itself: a directory that IS `home` must never be
/// treated as a project root, even if it happens to contain a marker —
/// this is what keeps the global `~/.agentflare` data dir from ever
/// being mistaken for a per-repo link.
#[test]
fn find_root_from_never_resolves_to_home_itself() {
    let home = tempfile::tempdir().unwrap();
    // Stands in for the real global data dir at ~/.agentflare.
    std::fs::create_dir_all(home.path().join(".agentflare")).unwrap();
    let start = home.path().join("some_project");
    std::fs::create_dir_all(&start).unwrap();

    // `start` itself has no marker, and home — one level up — does. If
    // the walk checked markers at `home`, this would return `home`. It
    // must instead stop short of ever inspecting `home` and fall back
    // to `start`.
    assert_eq!(AgentflareMcp::find_root_from(&start, home.path()), start);
}

// No test for the "nothing found anywhere above" fallback: `find_root_from`
// walks all the way to the filesystem root, so a tempdir-based test would
// depend on what markers happen to exist above the OS temp directory on
// whatever machine runs this — not a property this test can control. The
// fallback itself is a single trivial `None => return start`.
#[test]
fn item_get_resolves_bare_and_hash_prefixed_sequence_id() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let uuid = created["id"].as_str().unwrap().to_string();
    let seq = created["sequence_id"].as_i64().unwrap();

    let by_bare_seq: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "get".into(),
            id: Some(seq.to_string()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(by_bare_seq["id"], uuid);

    let by_hash_seq: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "get".into(),
            id: Some(format!("#{seq}")),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(by_hash_seq["id"], uuid);

    let by_uuid: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "get".into(),
            id: Some(uuid.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(by_uuid["id"], uuid);
}

#[test]
fn item_get_unknown_sequence_id_returns_not_found() {
    let (_tmp, s) = harness();
    let err = s
        .item(Parameters(ItemRequest {
            action: "get".into(),
            id: Some("999999".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}
