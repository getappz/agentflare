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

#[test]
fn item_cancel_clears_dispatch_lifecycle_labels() {
    // item #225: `run_discovery_tick` picks dispatch candidates by label
    // alone with no state check, so a cancelled item that still carries
    // `ready-for-work` (or a stale `dispatched`/`needs-manual-dispatch`
    // from a prior attempt) kept getting redispatched forever.
    let (tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let project_id = created["project_id"].as_str().unwrap().to_string();

    let (ready_id, dispatched_id) = {
        let conn = backend_conn(&tmp);
        let workspace_id = agentflare_backend::project::get(&conn, &project_id)
            .unwrap()
            .workspace_id;
        let mk = |name: &str| {
            agentflare_backend::label::create(
                &conn,
                agentflare_backend::label::CreateLabel {
                    project_id: Some(project_id.clone()),
                    workspace_id: workspace_id.clone(),
                    name: name.into(),
                    color: None,
                    parent_id: None,
                    sort_order: None,
                    external_source: None,
                    external_id: None,
                },
            )
            .unwrap()
            .id
        };
        let ready_id = mk("ready-for-work");
        let dispatched_id = mk("dispatched");
        agentflare_backend::item::add_label(&conn, &item_id, &ready_id).unwrap();
        agentflare_backend::item::add_label(&conn, &item_id, &dispatched_id).unwrap();
        (ready_id, dispatched_id)
    };

    s.item(Parameters(ItemRequest {
        action: "cancel".into(),
        id: Some(item_id.clone()),
        ..Default::default()
    }))
    .unwrap();

    let conn = backend_conn(&tmp);
    let labels = agentflare_backend::item::list_labels(&conn, &item_id).unwrap();
    assert!(!labels.contains(&ready_id));
    assert!(!labels.contains(&dispatched_id));
}

#[test]
fn item_release_clears_assignee_agent_via_mcp() {
    // item #93: `item_release` used to call `agentflare_backend::claim::release`
    // directly, which only drops the lease row and leaves `assignee_agent`
    // pinned to the released owner -- so a claimed-then-released item stayed
    // permanently blocked to every other agent type. Assert the MCP handler
    // is wired to the composed `agentflare_backend::item::release` that also
    // clears the pin, not the raw lease-only primitive.
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
    assert!(
        s.with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id))
            .unwrap()
            .unwrap()
            .assignee_agent
            .is_some()
    );

    s.item(Parameters(ItemRequest {
        action: "release".into(),
        id: Some(item_id.clone()),
        ..Default::default()
    }))
    .unwrap();

    assert_eq!(
        s.with_backend_db(|conn| agentflare_backend::item::get(conn, &item_id))
            .unwrap()
            .unwrap()
            .assignee_agent,
        None,
        "release must clear assignee_agent, not just the lease row"
    );
}

///
/// `pub(crate)`: reused by `mcp_server::tests::mcp_with_claimed_item`, which
/// layers item-create+claim logic on top of this shared git-init/`AgentflareMcp`
/// scaffolding instead of reimplementing it.
pub(crate) fn claim_harness() -> (AgentflareMcp, tempfile::TempDir, tempfile::TempDir) {
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
    // Return `repo_dir` alongside `tmp` — dropping it here (as the previous
    // version did) deletes the git repo before any caller can run real git
    // operations against it; the caller must keep both `TempDir` guards
    // alive for the duration of the test.
    (s, tmp, repo_dir)
}

#[test]
fn item_update_assignee_to_different_agent_releases_old_claim() {
    let (s, _tmp, _repo_tmp) = claim_harness();
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

    // Derive a target agent guaranteed to differ from whatever agent this
    // test process auto-detects as (avoids collisions with the real
    // ambient AGENTFLARE_AGENT / agent-detector identity, e.g. "claude-code").
    let different_agent = format!("{}-other", crate::claims::agent_of(&owner));

    // Reassign to a different agent — should release the old claim.
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(item_id.clone()),
        assignee_agent: Some(different_agent),
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
    let (s, _tmp, _repo_tmp) = claim_harness();
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
fn item_claim_blocked_by_plan() {
    // Same environment-dependent trap as
    // end_to_end_plan_gate_blocks_then_unblocks_claim: the create call below
    // sets assignee_agent="claude-code" so plan_required's claimability
    // check accepts it, so the final claim() must come from that same
    // identity or claim()'s handoff freeze (BlockedByAssignee) blocks it.
    // Pin the owner instead of relying on ambient agent-detection.
    crate::claims::with_owner_override("claude-code:test", || {
        item_claim_blocked_by_plan_inner();
    });
}

fn item_claim_blocked_by_plan_inner() {
    let (s, tmp, _repo_tmp) = claim_harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("gated item".into()),
            assignee_agent: Some("claude-code".into()),
            metadata: Some(serde_json::json!({"plan_required": true})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let blocked: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(blocked["status"], "blocked_by_plan");
    assert_eq!(blocked["plan_status"], "none");

    // Approve the plan via a direct backend update (bypassing the MCP
    // layer, which has no `submit_plan`/approval action yet — that's a
    // later task in this plan).
    let conn = backend_conn(&tmp);
    agentflare_backend::item::update(
        &conn,
        &item_id,
        agentflare_backend::item::UpdateItem {
            metadata: Some(r#"{"plan_required":true,"plan_status":"approved"}"#.into()),
            ..Default::default()
        },
    )
    .unwrap();
    drop(conn);

    let acquired: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(acquired["status"], "acquired");
}

#[test]
fn submit_plan_sets_pending_and_clears_prior_rejection() {
    // This test deliberately exercises the `plan_approver` == "human" default,
    // which makes `item_submit_plan` call `notify_plan_approval_gate`. That
    // reads the vault -- under the REAL $HOME without this wrapper -- and can
    // fire a REAL Telegram approve card on a developer's machine whose vault is
    // unlocked and notify chat configured. `with_temp_home` points the vault at
    // a throwaway directory, same as the Task 6 supervisor tests
    // (`handle_telegram_callback_approves_a_pending_plan`). Preferred over
    // passing `plan_approver: Some("agent")`, which would stop testing the
    // "human" default this test exists to assert (item #573 final review).
    crate::paths::test_support::with_temp_home(|| {
        let (tmp, s) = harness();
        let created: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "create".into(),
                name: Some("gated item".into()),
                assignee_agent: Some("claude-code".into()),
                metadata: Some(serde_json::json!({
                    "plan_required": true,
                    "plan_rejection_reason": "stale reason from a prior round",
                })),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        let item_id = created["id"].as_str().unwrap().to_string();

        let submitted: serde_json::Value = serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "submit_plan".into(),
                id: Some(item_id.clone()),
                plan_asset_id: Some("asset-1".into()),
                ..Default::default()
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(submitted["status"], "pending");
        assert_eq!(submitted["plan_asset_id"], "asset-1");
        // No plan_approver was passed and none was already on the item's
        // metadata, so submit_plan must fall back to "human".
        assert_eq!(submitted["plan_approver"], "human");

        let conn = backend_conn(&tmp);
        let item = agentflare_backend::item::get(&conn, &item_id).unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
        assert_eq!(metadata["plan_status"], "pending");
        assert_eq!(metadata["plan_asset_id"], "asset-1");
        assert!(
            metadata["plan_rejection_reason"].is_null(),
            "submit_plan must clear a stale rejection reason: {metadata}"
        );
    });
}

/// Creates a `plan_required` item already sitting at `plan_status = "pending"`
/// with the given `plan_approver`, and returns its id -- the exact state both
/// self-approval tests below need. Reaches "pending" via a real `submit_plan`
/// call rather than seeding `plan_status` straight into `create`'s metadata --
/// `create`/`update` strip that (and every other plan-transition field) from
/// caller metadata now, since only `submit_plan`/`approve_plan`/`reject_plan`
/// may set it (CodeRabbit finding on item #573's PR).
fn pending_plan_item(s: &AgentflareMcp, approver: &str) -> String {
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("human-gated plan".into()),
            assignee_agent: Some("claude-code".into()),
            metadata: Some(serde_json::json!({
                "plan_required": true,
                "plan_approver": approver,
            })),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    s.item(Parameters(ItemRequest {
        action: "submit_plan".into(),
        id: Some(item_id.clone()),
        plan_asset_id: Some("asset-1".into()),
        ..Default::default()
    }))
    .unwrap();
    item_id
}

/// Item #573 final review, Fix 1: the public, agent-callable `approve_plan`
/// must REFUSE a `plan_approver == "human"` item. Without this an agent that
/// hit `blocked_by_plan` could submit_plan + approve_plan itself and walk
/// straight through the gate.
#[test]
fn public_approve_plan_refuses_a_human_approver_item() {
    let (tmp, s) = harness();
    let item_id = pending_plan_item(&s, "human");

    let err = s
        .item(Parameters(ItemRequest {
            action: "approve_plan".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(
        err.message.contains("human approval"),
        "error must say why an agent can't approve: {}",
        err.message
    );

    let conn = backend_conn(&tmp);
    let item = agentflare_backend::item::get(&conn, &item_id).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
    assert_eq!(
        metadata["plan_status"], "pending",
        "a refused approval must not move the gate: {metadata}"
    );
}

/// CodeRabbit finding on item #573's PR: `submit_plan` must not let a caller
/// downgrade a stored `plan_approver == "human"` to `"agent"` and then
/// self-approve through the public route -- that would defeat Fix 1 above
/// entirely. Item starts at `plan_status = "none"` (not yet submitted) so
/// this exercises the real attack shape: hit `blocked_by_plan`, then try to
/// submit_plan with an overriding `plan_approver` before approve_plan.
#[test]
fn submit_plan_cannot_downgrade_a_stored_human_approver() {
    let (tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("human-gated, not yet submitted".into()),
            assignee_agent: Some("claude-code".into()),
            metadata: Some(serde_json::json!({
                "plan_required": true,
                "plan_approver": "human",
            })),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let submitted: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "submit_plan".into(),
            id: Some(item_id.clone()),
            plan_asset_id: Some("asset-downgrade-attempt".into()),
            plan_approver: Some("agent".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        submitted["plan_approver"], "human",
        "a stored human approver must survive submit_plan regardless of the caller's override"
    );

    let err = s
        .item(Parameters(ItemRequest {
            action: "approve_plan".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

    let conn = backend_conn(&tmp);
    let item = agentflare_backend::item::get(&conn, &item_id).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
    assert_eq!(
        metadata["plan_status"], "pending",
        "the downgrade-then-self-approve attempt must not unblock the gate: {metadata}"
    );
}

/// The other half of Fix 1: the channel-only route (a human's Telegram tap,
/// routed by `supervisor::handle_telegram_callback`) is the ONE way the same
/// item does get approved.
#[test]
fn channel_approve_plan_succeeds_for_a_human_approver_item() {
    let (tmp, s) = harness();
    let item_id = pending_plan_item(&s, "human");

    let approved: serde_json::Value = serde_json::from_str(
        &s.item_approve_plan_via_channel(ItemRequest {
            action: "approve_plan".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(approved["status"], "approved");

    let conn = backend_conn(&tmp);
    let item = agentflare_backend::item::get(&conn, &item_id).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
    assert_eq!(metadata["plan_status"], "approved");
}

/// Live incident, item #281: a human approved a plan, the supervisor
/// dispatched a job off that state, and within minutes the approval record
/// was gone -- `plan_status` back to unset, `plan_approved_at`/
/// `plan_approved_by` wiped, because a later unrelated metadata write (e.g.
/// `work_item_pipeline::persist_run_id` recording the dispatch's
/// `workflow_run_id`) went through `item(action="update")`, which used to
/// unconditionally strip the plan-transition fields out of ANY outgoing
/// metadata -- including a write that was only ever trying to add an
/// unrelated key on top of the item's own current metadata. An update that
/// never mentions the plan-gate fields at all must leave the approval intact.
#[test]
fn update_with_unrelated_metadata_does_not_erase_an_existing_approval() {
    let (tmp, s) = harness();
    let item_id = pending_plan_item(&s, "human");
    s.item_approve_plan_via_channel(ItemRequest {
        action: "approve_plan".into(),
        id: Some(item_id.clone()),
        ..Default::default()
    })
    .unwrap();

    // Mirrors `persist_run_id`'s own shape: read-current, patch in one
    // unrelated key, write the whole object back.
    let conn = backend_conn(&tmp);
    let current = agentflare_backend::item::get(&conn, &item_id)
        .unwrap()
        .metadata;
    drop(conn);
    let mut merged: serde_json::Value = serde_json::from_str(&current).unwrap();
    merged["workflow_run_id"] = serde_json::json!("01a0b3ad-test-run");
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(item_id.clone()),
        metadata: Some(merged),
        ..Default::default()
    }))
    .unwrap();

    let conn = backend_conn(&tmp);
    let item = agentflare_backend::item::get(&conn, &item_id).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
    assert_eq!(
        metadata["plan_status"], "approved",
        "an unrelated metadata write must not revert the approval: {metadata}"
    );
    assert!(
        metadata.get("plan_approved_by").is_some(),
        "plan_approved_by must survive an unrelated metadata write: {metadata}"
    );
    assert!(
        metadata.get("plan_approved_at").is_some(),
        "plan_approved_at must survive an unrelated metadata write: {metadata}"
    );
    assert_eq!(metadata["workflow_run_id"], "01a0b3ad-test-run");
}

/// Item #300 code review finding: `restore_plan_transition_fields` forces
/// the item's *current* `plan_status` back onto outgoing metadata to stop an
/// unrelated write from erasing an approval (the test above) -- but that
/// must not also defeat a genuine resubmission. Attaching a *new*
/// `plan_asset_id` to an already-`"approved"` item is exactly the shape
/// `merge_submitted_plan` exists to reset to `"pending"`; if the new plan
/// silently inherited the old approval instead, it would reach dispatch
/// without ever being reviewed.
#[test]
fn update_attaching_a_new_plan_asset_id_resets_an_approved_item_to_pending() {
    let (tmp, s) = harness();
    let item_id = pending_plan_item(&s, "human");
    s.item_approve_plan_via_channel(ItemRequest {
        action: "approve_plan".into(),
        id: Some(item_id.clone()),
        ..Default::default()
    })
    .unwrap();

    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(item_id.clone()),
        plan_asset_id: Some("asset-2-revised".into()),
        ..Default::default()
    }))
    .unwrap();

    let conn = backend_conn(&tmp);
    let item = agentflare_backend::item::get(&conn, &item_id).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
    assert_eq!(
        metadata["plan_status"], "pending",
        "a revised plan must go back to pending for re-review, not inherit the old approval: {metadata}"
    );
    assert_eq!(metadata["plan_asset_id"], "asset-2-revised");

    // Re-attaching the *same* asset id again is a no-op, same as before.
    s.item_approve_plan_via_channel(ItemRequest {
        action: "approve_plan".into(),
        id: Some(item_id.clone()),
        ..Default::default()
    })
    .unwrap();
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(item_id.clone()),
        plan_asset_id: Some("asset-2-revised".into()),
        ..Default::default()
    }))
    .unwrap();
    let conn = backend_conn(&tmp);
    let item = agentflare_backend::item::get(&conn, &item_id).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
    assert_eq!(
        metadata["plan_status"], "approved",
        "re-attaching the same plan_asset_id must not reset an approval that already covers it: {metadata}"
    );
}

/// Item #300 code review, second pass: `merge_submitted_plan` resets
/// `plan_status` on resubmission but must also clear a stale
/// `plan_rejection_reason` left over from a prior rejection, mirroring
/// `item_submit_plan`'s own patch -- otherwise a freshly-`"pending"`,
/// unreviewed plan sits next to a rejection reason that no longer applies
/// to it.
#[test]
fn update_attaching_a_new_plan_asset_id_after_rejection_clears_the_stale_reason() {
    let (tmp, s) = harness();
    let item_id = pending_plan_item(&s, "human");
    s.item(Parameters(ItemRequest {
        action: "reject_plan".into(),
        id: Some(item_id.clone()),
        reason: Some("needs more detail on rollback".into()),
        ..Default::default()
    }))
    .unwrap();

    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(item_id.clone()),
        plan_asset_id: Some("asset-2-revised".into()),
        ..Default::default()
    }))
    .unwrap();

    let conn = backend_conn(&tmp);
    let item = agentflare_backend::item::get(&conn, &item_id).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
    assert_eq!(metadata["plan_status"], "pending");
    assert_eq!(metadata["plan_asset_id"], "asset-2-revised");
    assert!(
        metadata
            .get("plan_rejection_reason")
            .is_none_or(|v| v.is_null()),
        "a resubmission must clear the previous rejection reason: {metadata}"
    );
}

/// Item #573 final review, Fix 5: an explicit `plan_approver` override is an
/// explicit gate choice ("gate this, but let an agent sign off") and must
/// survive the urgent/high default policy, which previously only looked for
/// `plan_required`.
#[test]
fn create_with_explicit_plan_approver_is_not_overridden() {
    let (_tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("urgent item, agent-approved".into()),
            priority: Some("urgent".into()),
            assignee_agent: Some("claude-code".into()),
            metadata: Some(serde_json::json!({"plan_approver": "agent"})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(created["metadata"].as_str().unwrap()).unwrap();
    assert_eq!(
        metadata["plan_approver"], "agent",
        "an explicit plan_approver must not be clobbered to \"human\": {metadata}"
    );
    // The policy is skipped wholesale when either key is explicit, so
    // plan_required is left exactly as the caller left it (absent here).
    assert!(
        metadata.get("plan_required").is_none(),
        "the default-gate patch must not touch plan_required either: {metadata}"
    );
}

/// CodeRabbit finding on item #573's PR: `create`/`update` must not accept
/// the plan-gate TRANSITION fields (`plan_status`, `plan_approved_by`,
/// `plan_approved_at`, `plan_rejection_reason`) straight out of caller
/// metadata -- only `submit_plan`/`approve_plan`/`reject_plan` may set them.
/// Without stripping, a caller could create an item already sitting at
/// `plan_status: "approved"`, skipping the gate lifecycle entirely.
/// `plan_required`/`plan_approver` (the caller's actual gating choice) must
/// still pass through untouched.
#[test]
fn create_strips_plan_transition_fields_from_caller_metadata() {
    let (_tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("smuggled approval attempt".into()),
            assignee_agent: Some("claude-code".into()),
            metadata: Some(serde_json::json!({
                "plan_required": true,
                "plan_approver": "human",
                "plan_status": "approved",
                "plan_approved_by": "attacker",
                "plan_approved_at": 1_700_000_000,
            })),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(created["metadata"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["plan_required"], true);
    assert_eq!(metadata["plan_approver"], "human");
    assert!(
        metadata.get("plan_status").is_none(),
        "create must not accept a caller-supplied plan_status: {metadata}"
    );
    assert!(
        metadata.get("plan_approved_by").is_none(),
        "create must not accept a caller-supplied plan_approved_by: {metadata}"
    );
    assert!(
        metadata.get("plan_approved_at").is_none(),
        "create must not accept a caller-supplied plan_approved_at: {metadata}"
    );
}

/// Same protection as the `create` test above, but for `update` -- an agent
/// updating an already-gated item's unrelated metadata (e.g. `size`) must
/// not be able to smuggle `plan_status: "approved"` into the same call.
#[test]
fn update_strips_plan_transition_fields_from_caller_metadata() {
    let (_tmp, s) = harness();
    let item_id = pending_plan_item(&s, "human");

    let updated: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "update".into(),
            id: Some(item_id),
            metadata: Some(serde_json::json!({
                "plan_required": true,
                "plan_approver": "human",
                "plan_status": "approved",
            })),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(updated["metadata"].as_str().unwrap()).unwrap();
    // The item's real state (set by `submit_plan` inside `pending_plan_item`)
    // is "pending" -- the write must restore that true current value, not
    // just delete the key, or a later good-faith merge (e.g.
    // `work_item_pipeline::persist_run_id` patching in `workflow_run_id`)
    // would silently erase a real approval the same way (item #281).
    assert_eq!(
        metadata.get("plan_status").and_then(|v| v.as_str()),
        Some("pending"),
        "update must not accept a caller-supplied plan_status, and must restore the item's \
         real current one instead of dropping it: {metadata}"
    );
}

#[test]
fn approve_plan_requires_pending_status() {
    let (_tmp, s) = harness();
    // Never submitted -- no plan_status at all on the item's metadata.
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create("ungated item")))
            .unwrap(),
    )
    .unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let err = s
        .item(Parameters(ItemRequest {
            action: "approve_plan".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn reject_plan_sets_rejected_and_records_reason() {
    let (tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("pending-plan item".into()),
            assignee_agent: Some("claude-code".into()),
            metadata: Some(serde_json::json!({
                "plan_required": true,
            })),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    // Reach plan_status = "pending" via a real submit_plan call --
    // create/update strip a caller-supplied plan_status now (CodeRabbit
    // finding on item #573's PR).
    s.item(Parameters(ItemRequest {
        action: "submit_plan".into(),
        id: Some(item_id.clone()),
        plan_asset_id: Some("asset-1".into()),
        ..Default::default()
    }))
    .unwrap();

    let rejected: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "reject_plan".into(),
            id: Some(item_id.clone()),
            reason: Some("needs more detail on rollback".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(rejected["status"], "rejected");

    let conn = backend_conn(&tmp);
    let item = agentflare_backend::item::get(&conn, &item_id).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
    assert_eq!(metadata["plan_status"], "rejected");
    assert_eq!(
        metadata["plan_rejection_reason"],
        "needs more detail on rollback"
    );
}

#[test]
fn create_with_urgent_priority_auto_gates_plan_required() {
    let (_tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("urgent item".into()),
            priority: Some("urgent".into()),
            assignee_agent: Some("claude-code".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(created["metadata"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["plan_required"], true);
    assert_eq!(metadata["plan_approver"], "human");
}

#[test]
fn create_with_explicit_plan_required_false_is_not_overridden() {
    let (_tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("urgent item, opted out".into()),
            priority: Some("urgent".into()),
            metadata: Some(serde_json::json!({"plan_required": false})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(created["metadata"].as_str().unwrap()).unwrap();
    // Explicit override wins over the default urgent/high auto-gate rule.
    assert_eq!(metadata["plan_required"], false);
}

/// Item #628 live incident (2026-09-22): `plan_required: true` with neither
/// an `assignee_agent` nor an already-submitted plan is permanently
/// unclaimable -- no agent will ever auto-dispatch it to write a plan, and
/// there's nothing yet for a human to approve either. `create` must refuse
/// this combination outright instead of silently accepting a dead-end item.
#[test]
fn create_rejects_plan_required_with_no_assignee_and_no_plan() {
    let (_tmp, s) = harness();
    let err = s
        .item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("dead-end gated item".into()),
            metadata: Some(serde_json::json!({"plan_required": true, "plan_approver": "human"})),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(
        err.message.contains("assignee_agent"),
        "error must name the fix: {}",
        err.message
    );
}

/// The normal dispatch-then-plan flow (#595, #610): an assignee is set, so
/// the assigned agent will be told to submit a plan on claim. Must still be
/// allowed.
#[test]
fn create_accepts_plan_required_with_assignee_agent_and_no_plan() {
    let (_tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("gated item, assigned".into()),
            assignee_agent: Some("claude-code".into()),
            metadata: Some(serde_json::json!({"plan_required": true, "plan_approver": "human"})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(created["metadata"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["plan_required"], true);
}

/// A plan written up front (spec first, assignment TBD): `plan_asset_id`
/// provided in the same `create` call also unblocks the gate, even with no
/// `assignee_agent`. It must also actually submit the plan -- `plan_asset_id`
/// stored and `plan_status` set to "pending" -- not just satisfy the
/// claimability check while leaving the item permanently un-approvable
/// (item #289 live incident: item #281 got stuck exactly this way, since
/// `approve_plan` refuses forever once `plan_status` is absent).
#[test]
fn create_accepts_plan_required_with_plan_asset_id_and_no_assignee() {
    let (_tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("gated item, spec up front".into()),
            plan_asset_id: Some("asset-spec-1".into()),
            metadata: Some(serde_json::json!({"plan_required": true, "plan_approver": "human"})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(created["metadata"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["plan_required"], true);
    assert_eq!(metadata["plan_asset_id"], "asset-spec-1");
    assert_eq!(metadata["plan_status"], "pending");
}

/// Same as the `create` case above, but reached via `update` on an
/// already-existing ungated item -- and via `plan_asset_id` embedded
/// directly in the `metadata` blob rather than the request's top-level
/// field, the other shape that used to leave `plan_status` unset.
#[test]
fn update_accepts_plan_required_with_plan_asset_id_and_no_assignee() {
    let (_tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create("ungated item")))
            .unwrap(),
    )
    .unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let updated: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "update".into(),
            id: Some(item_id),
            metadata: Some(serde_json::json!({
                "plan_required": true,
                "plan_approver": "human",
                "plan_asset_id": "asset-spec-2",
            })),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(updated["metadata"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["plan_required"], true);
    assert_eq!(metadata["plan_asset_id"], "asset-spec-2");
    assert_eq!(metadata["plan_status"], "pending");
}

/// Same dead-end check as `create`'s, but reached via `update` -- gating an
/// item that has no assignee and no submitted plan must be refused there
/// too, not just at creation.
#[test]
fn update_rejects_plan_required_with_no_assignee_and_no_plan() {
    let (_tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create("ungated item")))
            .unwrap(),
    )
    .unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let err = s
        .item(Parameters(ItemRequest {
            action: "update".into(),
            id: Some(item_id),
            metadata: Some(serde_json::json!({"plan_required": true, "plan_approver": "human"})),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

/// `update` setting `plan_required` and `assignee_agent` together in the same
/// call must be allowed -- same dispatch-then-plan flow as `create`'s.
#[test]
fn update_accepts_plan_required_with_assignee_agent_in_same_call() {
    let (_tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(empty_item_create("ungated item")))
            .unwrap(),
    )
    .unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let updated: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "update".into(),
            id: Some(item_id),
            assignee_agent: Some("claude-code".into()),
            metadata: Some(serde_json::json!({"plan_required": true, "plan_approver": "human"})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let metadata: serde_json::Value =
        serde_json::from_str(updated["metadata"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["plan_required"], true);
}

#[test]
fn update_priority_to_urgent_auto_gates_plan_required() {
    let (tmp, s) = harness();
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("ordinary item".into()),
            assignee_agent: Some("claude-code".into()),
            metadata: Some(serde_json::json!({"size": "M"})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(item_id.clone()),
        priority: Some("urgent".into()),
        ..Default::default()
    }))
    .unwrap();

    let conn = backend_conn(&tmp);
    let item = agentflare_backend::item::get(&conn, &item_id).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&item.metadata).unwrap();
    assert_eq!(metadata["plan_required"], true);
    assert_eq!(metadata["plan_approver"], "human");
    // The pre-existing, unrelated metadata key must survive the merge --
    // `UpdateItem::metadata` replaces the column wholesale, so this proves
    // the default-gate patch was merged onto the item's current metadata
    // rather than clobbering it.
    assert_eq!(metadata["size"], "M");
}

/// Full feature end-to-end: an item auto-gated at creation cannot be
/// claimed until a submitted plan is approved.
#[test]
fn end_to_end_plan_gate_blocks_then_unblocks_claim() {
    // Step 1 sets `assignee_agent: "claude-code"` so plan_required's new
    // claimability check accepts it; the final claim in step 6 must then
    // come from that same agent identity or `claim()`'s handoff freeze
    // (`BlockedByAssignee`) blocks it. Pin the owner explicitly instead of
    // relying on ambient agent-detection (`owner_id()` falls back to
    // `flare_process::agent_name()`, which resolves to "claude-code" only
    // when actually running inside Claude Code -- a bare CI runner detects
    // nothing and falls back to "cli", which doesn't match).
    crate::claims::with_owner_override("claude-code:test", || {
        end_to_end_plan_gate_blocks_then_unblocks_claim_inner();
    });
}

fn end_to_end_plan_gate_blocks_then_unblocks_claim_inner() {
    let (s, _tmp, _repo_tmp) = claim_harness();

    // 1. Create with priority="urgent" plus an explicit plan_approver="agent"
    //    -> auto-gated plan_required=true, but approvable through the public
    //    API surface this test exercises. Priority="urgent" alone would
    //    auto-gate to plan_approver="human" (see default_policy), which can
    //    only be unblocked via the Telegram channel route -- not reachable
    //    through item()'s public dispatch, and deliberately so: submit_plan
    //    can no longer downgrade a stored "human" approver to "agent" (that
    //    was a self-approval bypass this test used to exercise unknowingly;
    //    see approve_plan_refuses_self_approval_on_human_gated_item for the
    //    coverage of that refusal).
    let created: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("urgent gated item".into()),
            priority: Some("urgent".into()),
            assignee_agent: Some("claude-code".into()),
            metadata: Some(serde_json::json!({"plan_required": true, "plan_approver": "agent"})),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let metadata: serde_json::Value =
        serde_json::from_str(created["metadata"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["plan_required"], true);

    // 2. claim -> blocked_by_plan, plan_status "none" (never submitted).
    let blocked: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(blocked["status"], "blocked_by_plan");
    assert_eq!(blocked["plan_status"], "none");

    // 3. submit_plan -> plan_status "pending", plan_approver stays "agent"
    //    (the stored value from creation; submit_plan no longer accepts a
    //    caller override that would weaken a "human" gate, but "agent" was
    //    never "human" here so there's nothing to weaken).
    let submitted: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "submit_plan".into(),
            id: Some(item_id.clone()),
            plan_asset_id: Some("asset-e2e".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(submitted["status"], "pending");
    assert_eq!(submitted["plan_approver"], "agent");

    // 4. claim again -> still blocked, plan_status "pending".
    let still_blocked: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(still_blocked["status"], "blocked_by_plan");
    assert_eq!(still_blocked["plan_status"], "pending");

    // 5. approve_plan -> plan_status "approved".
    let approved: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "approve_plan".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(approved["status"], "approved");

    // 6. claim -> acquired.
    let acquired: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(acquired["status"], "acquired");
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
    let names: Vec<&str> = listed["items"]
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
    let mut names: Vec<&str> = defaulted["items"]
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
    let mut names2: Vec<&str> = explicit["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    names2.sort_unstable();
    assert_eq!(names2, vec!["Others", "Unassigned"]);
}

#[test]
fn item_list_includes_a_claimed_item_for_its_own_agent() {
    // item #66: `item::claim` stores the claim owner's raw id
    // (`<agent>:<instance>`), while the default assignee filter matches the
    // canonical agent name. Before the fix, exact-string matching dropped a
    // claimed item from its own agent's `list`, so an in_review item carrying
    // a (stale) claim was invisible until the claim was released.
    let tmp = tempfile::tempdir().unwrap();
    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        agent: Some("claude-code".into()),
        ..Default::default()
    };

    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Mine"))).unwrap()).unwrap();
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(created["id"].as_str().unwrap().to_string()),
        assignee_agent: Some("claude-code:job-538".into()),
        ..Default::default()
    }))
    .unwrap();

    serde_json::from_str::<serde_json::Value>(
        &s.item(Parameters(empty_item_create("Unassigned"))).unwrap(),
    )
    .unwrap();

    let listed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let names: Vec<&str> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"Mine"),
        "a claimed item must stay visible to its own agent's default list, got {names:?}"
    );
    assert!(names.contains(&"Unassigned"));
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
    let names: Vec<&str> = listed["items"]
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

/// #377: `parent_id` on update was accepted and silently discarded — the
/// response even echoed the *old* parent, so only a diff of the returned body
/// against what was sent could catch it.
#[test]
fn item_update_persists_parent_id_and_accepts_a_sequence_id() {
    let (_tmp, s) = harness();
    let update_parent = |id: &str, parent: &str| -> serde_json::Value {
        serde_json::from_str(
            &s.item(Parameters(ItemRequest {
                action: "update".into(),
                id: Some(id.to_string()),
                parent_id: Some(parent.to_string()),
                ..Default::default()
            }))
            .unwrap_or_else(|e| panic!("update parent_id={parent:?} failed: {e:?}")),
        )
        .unwrap()
    };
    let epic: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Epic"))).unwrap()).unwrap();
    let child: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Child"))).unwrap()).unwrap();
    let epic_id = epic["id"].as_str().unwrap().to_string();
    let epic_seq = epic["sequence_id"].as_i64().unwrap();
    let child_id = child["id"].as_str().unwrap().to_string();

    // By UUID, then re-read to prove it is the row that changed, not just
    // the response body.
    let updated = update_parent(&child_id, &epic_id);
    assert_eq!(updated["parent_id"], serde_json::json!(epic_id));
    let fetched: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "get".into(),
            id: Some(child_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(fetched["parent_id"], serde_json::json!(epic_id));

    // A sequence_id names the same parent.
    update_parent(&child_id, "");
    let by_seq = update_parent(&child_id, &format!("#{epic_seq}"));
    assert_eq!(by_seq["parent_id"], serde_json::json!(epic_id));

    // An empty string detaches; an omitted parent_id leaves it alone.
    let renamed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "update".into(),
            id: Some(child_id.clone()),
            name: Some("Child renamed".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(renamed["parent_id"], serde_json::json!(epic_id));
    assert_eq!(
        update_parent(&child_id, "")["parent_id"],
        serde_json::Value::Null
    );
}

#[test]
fn item_update_rejects_self_parent_cycles_and_unknown_parents() {
    let (_tmp, s) = harness();
    let epic: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Epic"))).unwrap()).unwrap();
    let child: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Child"))).unwrap()).unwrap();
    let epic_id = epic["id"].as_str().unwrap().to_string();
    let child_id = child["id"].as_str().unwrap().to_string();
    s.item(Parameters(ItemRequest {
        action: "update".into(),
        id: Some(child_id.clone()),
        parent_id: Some(epic_id.clone()),
        ..Default::default()
    }))
    .unwrap();

    for (id, parent) in [
        (&child_id, &child_id),           // self-parent
        (&epic_id, &child_id),            // cycle: epic -> child -> epic
        (&child_id, &"9999".to_string()), // unknown sequence_id
        (&child_id, &"nope-uuid".to_string()),
    ] {
        let err = s
            .item(Parameters(ItemRequest {
                action: "update".into(),
                id: Some(id.clone()),
                parent_id: Some(parent.clone()),
                ..Default::default()
            }))
            .unwrap_err();
        assert_eq!(
            err.code,
            rmcp::model::ErrorCode::INVALID_PARAMS,
            "parent_id {parent:?} should be rejected"
        );
    }
}

/// `create`'s `parent_id` must resolve a sequence_id the same way `update`'s
/// does (#375/#377) — otherwise it reaches the INSERT's FK column raw and
/// fails as an opaque "FOREIGN KEY constraint failed" instead of naming the
/// bad id.
#[test]
fn item_create_resolves_parent_id_sequence_id_and_rejects_unknown() {
    let (_tmp, s) = harness();
    let epic: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Epic"))).unwrap()).unwrap();
    let epic_id = epic["id"].as_str().unwrap().to_string();
    let epic_seq = epic["sequence_id"].as_i64().unwrap();

    let child: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Child".into()),
            parent_id: Some(format!("#{epic_seq}")),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(child["parent_id"], serde_json::json!(epic_id));

    let err = s
        .item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Orphan".into()),
            parent_id: Some("9999".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn item_groom_reads_size_and_flags_unestimated() {
    let (_tmp, s) = harness();
    let sized: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "create".into(),
            name: Some("Sized".into()),
            // Size "L" auto-gates `plan_required` (`default_policy`); an
            // assignee is required so the gate stays claimable.
            assignee_agent: Some("claude-code".into()),
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
    let shortlist_ids: Vec<String> = listed["items"]
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
    let names: Vec<&str> = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["B"]);
    assert_eq!(listed["total"], 3);
    assert_eq!(listed["offset"], 1);
    assert_eq!(listed["limit"], 1);
    assert_eq!(listed["next_offset"], 2);
    assert_eq!(listed["prev_offset"], 0);
}

#[test]
fn item_list_pagination_edges_out_of_range_offset_and_zero_limit() {
    let (_tmp, s) = harness();
    for name in ["A", "B", "C"] {
        s.item(Parameters(empty_item_create(name))).unwrap();
    }

    let past_end: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            limit: Some(1),
            offset: Some(100),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(past_end["items"].as_array().unwrap().len(), 0);
    assert_eq!(past_end["next_offset"], serde_json::Value::Null);
    assert_eq!(past_end["prev_offset"], 2);

    let zero_limit: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "list".into(),
            limit: Some(0),
            offset: Some(1),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(zero_limit["items"].as_array().unwrap().len(), 0);
    assert_eq!(zero_limit["next_offset"], serde_json::Value::Null);
    assert_eq!(zero_limit["prev_offset"], serde_json::Value::Null);
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
    let first = &listed["items"].as_array().unwrap()[0];
    assert_eq!(first["state"], "Backlog");
    assert_eq!(first["state_group"], "backlog");
    assert!(first.get("description").is_none());
    assert!(first.get("metadata").is_none());
    assert_eq!(listed["next_offset"], serde_json::Value::Null);
    assert_eq!(listed["prev_offset"], serde_json::Value::Null);
}

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
