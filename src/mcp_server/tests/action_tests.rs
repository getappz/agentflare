use super::*;

#[test]
fn item_comment_create_and_list_roundtrip() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let comment: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.clone()),
            body: Some("Hello, world!".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(comment["body"], "Hello, world!");
    assert!(comment["author_agent"].as_str().unwrap().contains(':'));

    let comments: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "list".into(),
            item_id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let arr = comments.as_array().unwrap();
    assert_eq!(arr.len(), 1);
    assert_eq!(arr[0]["body"], "Hello, world!");
}

#[test]
fn item_comment_rejects_empty_body() {
    let (_tmp, s) = harness();
    let err = s
        .comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some("item-1".into()),
            body: Some("".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn item_comment_edit_succeeds_when_latest_and_own_and_unclaimed_by_other() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let comment: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.clone()),
            body: Some("original".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let comment_id = comment["id"].as_str().unwrap().to_string();

    let updated: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "edit".into(),
            id: Some(comment_id.clone()),
            body: Some("edited".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(updated["body"], "edited");
}

#[test]
fn item_comment_edit_rejected_when_comment_not_found() {
    let (_tmp, s) = harness();
    let err = s
        .comment(Parameters(CommentRequest {
            action: "edit".into(),
            id: Some("nonexistent".into()),
            body: Some("edited".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn item_comment_edit_rejected_when_different_agent() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let comment_id = s
        .with_backend_db(|conn| {
            agentflare_backend::comment::create(conn, &item_id, "someone-else:1", "not mine")
                .unwrap()
                .id
        })
        .unwrap();

    let err = s
        .comment(Parameters(CommentRequest {
            action: "edit".into(),
            id: Some(comment_id),
            body: Some("edited".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(err.message.contains("own comments"));
}

#[test]
fn item_comment_edit_succeeds_across_sessions_of_same_agent() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    // Same agent, different session instance — e.g. a prior CLI
    // invocation, or an MCP server process that has since restarted.
    let agent = crate::claims::agent_of(&crate::claims::owner_id()).to_string();
    let earlier_session_author = format!("{agent}:some-earlier-session");

    let comment_id = s
        .with_backend_db(|conn| {
            agentflare_backend::comment::create(
                conn,
                &item_id,
                &earlier_session_author,
                "mine, from an earlier session",
            )
            .unwrap()
            .id
        })
        .unwrap();

    let updated: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "edit".into(),
            id: Some(comment_id),
            body: Some("edited".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(updated["body"], "edited");
}

#[test]
fn item_comment_edit_uses_id_tiebreak_when_timestamps_collide() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let first: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id.clone()),
            body: Some("first".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let first_id = first["id"].as_str().unwrap().to_string();

    let second: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id),
            body: Some("second".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let second_id = second["id"].as_str().unwrap().to_string();

    // ID tiebreak: nanoid is random, so determine "latest" at runtime.
    let (lower_id, higher_id) = if first_id > second_id {
        (second_id, first_id)
    } else {
        (first_id, second_id)
    };

    // Force both comments onto the same second-resolution timestamp.
    s.with_backend_db(|conn| {
        conn.execute(
            "UPDATE item_comments SET created_at = 1000, updated_at = 1000",
            [],
        )
        .unwrap();
    })
    .unwrap();

    let err = s
        .comment(Parameters(CommentRequest {
            action: "edit".into(),
            id: Some(lower_id),
            body: Some("edited".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);

    let updated: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "edit".into(),
            id: Some(higher_id),
            body: Some("edited".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(updated["body"], "edited");
}

#[test]
fn item_comment_delete_succeeds_when_latest_and_own() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let comment: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "create".into(),
            item_id: Some(item_id),
            body: Some("delete-me".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let comment_id = comment["id"].as_str().unwrap().to_string();

    let result: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "delete".into(),
            id: Some(comment_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["deleted"], true);
}

#[test]
fn item_claim_response_includes_worktree_path() {
    let tmp = tempfile::tempdir().unwrap();
    // Isolated temp repo — this test must never run real `git
    // worktree`/branch operations against the actual repository running
    // the test suite.
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

    let result: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["status"], "acquired");
    assert!(result.get("worktree_path").is_some());
    let path = result["worktree_path"].as_str().unwrap();
    assert!(std::path::Path::new(path).exists());
    // `next` is now a protocol-level decoration injected by
    // `call_tool`, not in the direct method output.
    assert!(result.get("next").is_none());
}

#[test]
fn item_claim_response_includes_worktree_error_instead_of_silently_omitting_it() {
    let tmp = tempfile::tempdir().unwrap();
    // Deliberately NOT a git repo — `git worktree add` must fail here, and
    // that failure must surface as `worktree_error` instead of just
    // vanishing (the bug this test guards against: `worktree_path` silently
    // missing with zero indication why, which read as an unexplained
    // claim/worktree deadlock).
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().to_path_buf();

    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root),
        ..Default::default()
    };

    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let result: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["status"], "acquired");
    assert!(
        result.get("worktree_path").is_none(),
        "worktree creation must have failed, so no path should be present"
    );
    assert!(
        result.get("worktree_error").is_some(),
        "a failed worktree creation must surface why, not just omit worktree_path"
    );
    assert!(!result["worktree_error"].as_str().unwrap().is_empty());
}

#[test]
fn item_done_without_new_commits_omits_pr_fields() {
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

    // No commits were made in the claimed worktree, so `done` has
    // nothing to push/PR — must not attempt a real push (no remote
    // configured on this throwaway repo).
    let result: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["done"], true);
    assert!(result.get("pr_url").is_none());
    assert!(result.get("next").is_none());
}

#[test]
fn item_rejects_unknown_action() {
    let (_tmp, s) = harness();
    let err = s
        .item(Parameters(ItemRequest {
            action: "nonexistent".into(),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn label_rejects_unknown_action() {
    let (_tmp, s) = harness();
    let err = s
        .label(Parameters(LabelRequest {
            action: "nonexistent".into(),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn label_create_list_update_delete_via_mcp() {
    let (_tmp, s) = harness();
    // create
    let created: serde_json::Value = serde_json::from_str(
        &s.label(Parameters(LabelRequest {
            action: "create".into(),
            name: Some("bug".into()),
            color: Some("#EF4444".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let id = created["id"].as_str().unwrap().to_string();
    assert_eq!(created["name"], "bug");
    assert_eq!(created["color"], "#EF4444");

    // list shows it
    let listed: serde_json::Value = serde_json::from_str(
        &s.label(Parameters(LabelRequest {
            action: "list".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["id"], id);

    // update renames + recolors
    let updated: serde_json::Value = serde_json::from_str(
        &s.label(Parameters(LabelRequest {
            action: "update".into(),
            id: Some(id.clone()),
            name: Some("defect".into()),
            color: Some("#F59E0B".into()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(updated["name"], "defect");
    assert_eq!(updated["color"], "#F59E0B");

    // delete
    let deleted: serde_json::Value = serde_json::from_str(
        &s.label(Parameters(LabelRequest {
            action: "delete".into(),
            id: Some(id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(deleted["deleted"], true);

    // list is now empty
    let after: serde_json::Value = serde_json::from_str(
        &s.label(Parameters(LabelRequest {
            action: "list".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(after.as_array().unwrap().is_empty());
}

#[test]
fn label_update_requires_id() {
    let (_tmp, s) = harness();
    let err = s
        .label(Parameters(LabelRequest {
            action: "update".into(),
            name: Some("x".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn item_add_label_rejects_foreign_project_label_via_mcp() {
    let (tmp, s) = harness();
    // Auto-provisions this repo's workspace + project.
    let item: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = item["id"].as_str().unwrap().to_string();

    // A label belonging to a completely separate workspace/project.
    let foreign_label_id = {
        let conn = backend_conn(&tmp);
        let ws = agentflare_backend::workspace::create(
            &conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "Other".into(),
                slug: "other".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = agentflare_backend::project::create(
            &conn,
            agentflare_backend::project::CreateProject {
                workspace_id: ws.id.clone(),
                name: "Other".into(),
                identifier: "OTH".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        agentflare_backend::label::create(
            &conn,
            agentflare_backend::label::CreateLabel {
                project_id: Some(proj.id),
                workspace_id: ws.id,
                name: "bug".into(),
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

    let err = s
        .item(Parameters(ItemRequest {
            action: "add_label".into(),
            id: Some(item_id),
            label_id: Some(foreign_label_id),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn label_update_and_delete_reject_foreign_project_label() {
    let (tmp, s) = harness();

    // A label in a separate workspace/project, not the repo's resolved project.
    let foreign_label_id = {
        let conn = backend_conn(&tmp);
        let ws = agentflare_backend::workspace::create(
            &conn,
            agentflare_backend::workspace::CreateWorkspace {
                name: "Other".into(),
                slug: "other".into(),
                owner_agent: None,
                item_label: None,
            },
        )
        .unwrap();
        let proj = agentflare_backend::project::create(
            &conn,
            agentflare_backend::project::CreateProject {
                workspace_id: ws.id.clone(),
                name: "Other".into(),
                identifier: "OTH".into(),
                external_source: None,
                external_id: None,
            },
        )
        .unwrap();
        agentflare_backend::label::create(
            &conn,
            agentflare_backend::label::CreateLabel {
                project_id: Some(proj.id),
                workspace_id: ws.id,
                name: "bug".into(),
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

    let upd = s
        .label(Parameters(LabelRequest {
            action: "update".into(),
            id: Some(foreign_label_id.clone()),
            name: Some("hijacked".into()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(upd.code, rmcp::model::ErrorCode::INVALID_PARAMS);

    let del = s
        .label(Parameters(LabelRequest {
            action: "delete".into(),
            id: Some(foreign_label_id.clone()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(del.code, rmcp::model::ErrorCode::INVALID_PARAMS);

    // The foreign label must survive both rejected attempts unchanged.
    let conn = backend_conn(&tmp);
    let survivor = agentflare_backend::label::get(&conn, &foreign_label_id).unwrap();
    assert_eq!(survivor.name, "bug");
}

#[test]
fn webhook_rejects_unknown_action() {
    let (_tmp, s) = harness();
    let err = s
        .webhook(Parameters(WebhookRequest {
            action: "nonexistent".into(),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn project_rejects_unknown_action() {
    let (_tmp, s) = harness();
    let err = s
        .project(Parameters(ProjectRequest {
            action: "nonexistent".into(),
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[test]
fn next_hint_claim_with_worktree_path() {
    let json = serde_json::json!({"status": "acquired", "worktree_path": "/tmp/wt"});
    let hint = next_hint("item", &json).unwrap();
    assert!(hint.contains("worktree_path"), "{}", hint);
}

#[test]
fn next_hint_done_with_pr_url() {
    let json = serde_json::json!({"done": true, "pr_url": "https://github.com/x/pull/1"});
    let hint = next_hint("item", &json).unwrap();
    assert!(hint.contains("review/merge"), "{}", hint);
}

#[test]
fn next_hint_handoff_always_returns_hint() {
    let json = serde_json::json!({"item_id": "abc", "recipient": "x"});
    let hint = next_hint("handoff", &json).unwrap();
    assert!(hint.contains("inbox"), "{}", hint);
}

#[test]
fn next_hint_unknown_tool_returns_none() {
    let json = serde_json::json!({"result": "ok"});
    assert!(next_hint("asset", &json).is_none());
}

#[test]
fn next_hint_item_without_trigger_fields_returns_none() {
    let json = serde_json::json!({"done": true});
    assert!(next_hint("item", &json).is_none());
}

#[test]
fn next_hint_non_object_input_returns_none() {
    assert!(next_hint("item", &serde_json::Value::String("text".into())).is_none());
}
