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
fn item_done_without_new_commits_leaves_the_item_unchanged() {
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
    // configured on this throwaway repo), and must not mark the item
    // completed either (item #48): nothing was actually delivered.
    let result: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["done"], false);
    assert_eq!(result["status"], "unchanged");
    assert!(result.get("pr_url").is_none());
    assert!(result.get("next").is_none());
}

#[test]
fn item_done_removes_its_own_clean_worktree() {
    // #420: `claim` provisions a worktree but nothing ever removed it,
    // leaving an orphan behind after every `done`.
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

    let claimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let worktree_path = std::path::PathBuf::from(claimed["worktree_path"].as_str().unwrap());
    assert!(
        worktree_path.exists(),
        "claim must have created the worktree"
    );

    s.item(Parameters(ItemRequest {
        action: "done".into(),
        id: Some(item_id),
        ..Default::default()
    }))
    .unwrap();

    assert!(
        !worktree_path.exists(),
        "a cleanly-finished item's worktree must be removed on done, not orphaned"
    );
}

#[test]
fn item_release_removes_its_own_clean_worktree() {
    // Item #335: `done`/`check_merge` were the only paths that ever cleaned
    // up a claimed worktree -- a plain `release` (an agent abandoning a
    // claim, or an item completed by hand outside the `done` flow) left it
    // orphaned forever.
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

    let claimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let worktree_path = std::path::PathBuf::from(claimed["worktree_path"].as_str().unwrap());
    assert!(
        worktree_path.exists(),
        "claim must have created the worktree"
    );

    let released: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "release".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(released["released"], true, "{released:?}");
    assert!(
        !worktree_path.exists(),
        "releasing a cleanly-finished claim must remove its worktree, not orphan it"
    );
}

#[test]
fn item_release_leaves_a_dirty_worktree_in_place() {
    // The same safety net `done` gets: uncommitted changes exist ONLY in
    // that checkout, so a release must never delete them out from under
    // whoever might resume the work.
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

    let claimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let worktree_path = std::path::PathBuf::from(claimed["worktree_path"].as_str().unwrap());
    std::fs::write(worktree_path.join("uncommitted.txt"), "not yet committed").unwrap();

    s.item(Parameters(ItemRequest {
        action: "release".into(),
        id: Some(item_id),
        ..Default::default()
    }))
    .unwrap();

    assert!(
        worktree_path.join("uncommitted.txt").exists(),
        "a dirty worktree must be left in place, not deleted out from under uncommitted work"
    );
}

#[test]
fn item_done_auto_commits_uncommitted_changes_instead_of_stranding_them() {
    // Item #57: a headless run that edited real files but never ran `git
    // commit` itself used to look identical, from the outside, to a
    // genuine no-op -- `done` reported success while the actual edits sat
    // stranded, uncommitted, in the worktree forever. `done` must now
    // commit any uncommitted changes itself before treating the claim as
    // having nothing to show for it.
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().to_path_buf();
    let run_git = |dir: &std::path::Path, args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
    };
    run_git(&repo_root, &["init", "-b", "master"]);
    run_git(&repo_root, &["config", "user.email", "test@test.com"]);
    run_git(&repo_root, &["config", "user.name", "Test"]);
    run_git(&repo_root, &["commit", "--allow-empty", "-m", "initial"]);

    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root.clone()),
        ..Default::default()
    };

    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let sequence_id = created["sequence_id"].as_i64().unwrap();

    let claimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let worktree_path = std::path::PathBuf::from(claimed["worktree_path"].as_str().unwrap());
    std::fs::write(worktree_path.join("forgot_to_commit.txt"), "real work").unwrap();

    // push: false avoids needing a real remote in this test -- the auto-
    // commit safety net itself doesn't depend on whether the result gets
    // pushed.
    let result: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id),
            summary: Some("Did the real work, just forgot to commit it.".into()),
            push: Some(false),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();

    assert_eq!(result["done"], true, "{result:?}");
    assert_eq!(result["status"], "completed");

    let branch = format!("task/{sequence_id}");
    let log = run_git(
        &repo_root,
        &["log", &branch, "--name-only", "--pretty=format:"],
    );
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("forgot_to_commit.txt"),
        "the agent's uncommitted edit must have landed in a real commit, not been dropped"
    );
}

#[test]
fn item_done_leaves_a_dirty_worktree_in_place_when_auto_commit_cannot_run() {
    // The remaining case cleanup must still refuse: when the auto-commit
    // safety net itself can't run (git commands failing in the checkout),
    // `done` must fall back to the old behavior -- leaving a dirty
    // worktree in place rather than guessing it's safe to delete.
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().to_path_buf();
    let run_git = |dir: &std::path::Path, args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
    };
    run_git(&repo_root, &["init", "-b", "master"]);
    run_git(&repo_root, &["config", "user.email", "test@test.com"]);
    run_git(&repo_root, &["config", "user.name", "Test"]);
    run_git(&repo_root, &["commit", "--allow-empty", "-m", "initial"]);

    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root),
        ..Default::default()
    };

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
    let worktree_path = std::path::PathBuf::from(claimed["worktree_path"].as_str().unwrap());
    std::fs::write(worktree_path.join("uncommitted.txt"), "not yet committed").unwrap();

    // Break the checkout's gitdir pointer so `git status`/`add`/`commit`
    // inside it all fail deterministically -- exercising the fallback path
    // rather than the happy one above, without depending on environment-
    // specific git author-identity fallbacks.
    std::fs::write(worktree_path.join(".git"), "gitdir: /nonexistent").unwrap();

    s.item(Parameters(ItemRequest {
        action: "done".into(),
        id: Some(item_id),
        ..Default::default()
    }))
    .unwrap();

    assert!(
        worktree_path.join("uncommitted.txt").exists(),
        "a worktree whose changes could not be auto-committed must be left in place"
    );
}

#[test]
fn item_done_with_push_false_never_pushes_the_branch() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().to_path_buf();
    let remote_dir = tempfile::tempdir().unwrap();
    let run_git = |dir: &std::path::Path, args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
    };
    run_git(remote_dir.path(), &["init", "--bare", "-b", "master"]);
    run_git(&repo_root, &["init", "-b", "master"]);
    run_git(&repo_root, &["config", "user.email", "test@test.com"]);
    run_git(&repo_root, &["config", "user.name", "Test"]);
    run_git(&repo_root, &["commit", "--allow-empty", "-m", "initial"]);
    run_git(
        &repo_root,
        &[
            "remote",
            "add",
            "origin",
            &remote_dir.path().to_string_lossy(),
        ],
    );
    run_git(&repo_root, &["push", "origin", "master"]);

    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root),
        ..Default::default()
    };

    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let sequence_id = created["sequence_id"].as_i64().unwrap();

    let claimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let worktree_path = std::path::PathBuf::from(claimed["worktree_path"].as_str().unwrap());
    std::fs::write(worktree_path.join("f.txt"), "x").unwrap();
    run_git(&worktree_path, &["add", "f.txt"]);
    run_git(&worktree_path, &["commit", "-m", "work"]);

    let result: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id),
            push: Some(false),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(result.get("pr_url").is_none());

    let branch = format!("task/{sequence_id}");
    let remote_refs = run_git(remote_dir.path(), &["branch", "--list", &branch]);
    assert!(
        String::from_utf8_lossy(&remote_refs.stdout)
            .trim()
            .is_empty(),
        "push: false must not push the branch to the remote"
    );
}

#[test]
fn item_done_with_a_resulting_pr_moves_to_in_review_not_completed() {
    // The regression this guards: state used to flip to "Completed" before
    // the push even ran, so an item could show done while its PR was still
    // unreviewed. Simulated here since producing a real pr_url needs live
    // GitHub credentials -- push a real commit to a real local remote and
    // fake an already-open PR by pre-seeding `find_existing`'s lookup is
    // out of reach without a GitHub mock, so this drives `mark_in_review`
    // directly the way `item_done` would once pr_url resolves, and asserts
    // the response shape a caller would see.
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

    let owner = crate::claims::owner_id();
    s.with_backend_db(|conn| {
        assert!(agentflare_backend::item::mark_in_review(conn, &item_id, &owner).unwrap());
    })
    .unwrap();

    let fetched: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "get".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        fetched["completed_at"].is_null(),
        "in_review must not be treated as completed"
    );

    // The lease must still be held -- a genuinely different agent must be
    // rejected mid-review. (The MCP `claim` action always uses this
    // process's own fixed owner_id, so re-claiming through it can't
    // simulate a different agent -- go straight at the backend, the same
    // way item.rs's own claim tests do.)
    s.with_backend_db(|conn| {
        match agentflare_backend::item::claim(conn, &item_id, "agent:2", crate::claims::now(), 3600)
            .unwrap()
        {
            agentflare_backend::item::ClaimOutcome::Held { .. } => {}
            other => panic!("expected Held while in_review, got {other:?}"),
        }
    })
    .unwrap();
}

#[test]
fn item_check_merge_is_a_noop_when_not_in_review() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();

    let result: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "check_merge".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["promoted"], false);
    assert_eq!(result["reason"], "item is not in_review");
}

#[test]
fn item_check_merge_leaves_an_in_review_item_alone_when_merge_status_is_unknown() {
    // No remote configured in this throwaway repo, so `is_pr_merged`
    // soft-fails to "not merged" -- check_merge must leave the item
    // exactly as it found it, not guess.
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
    let owner = crate::claims::owner_id();
    s.with_backend_db(|conn| {
        assert!(agentflare_backend::item::mark_in_review(conn, &item_id, &owner).unwrap());
    })
    .unwrap();

    let result: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "check_merge".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["promoted"], false);
    assert_eq!(result["reason"], "PR not merged yet");

    let fetched: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "get".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(fetched["completed_at"].is_null(), "must still be in_review");
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

// --- item #83: release/done must not silently no-op on a caller/assignee
// identity mismatch -- a live claim held by someone else is a real
// conflict (error clearly), an abandoned one must not be permanently
// un-releasable just because the original claiming instance is gone. ---

fn seed_claim(s: &AgentflareMcp, item_id: &str, owner: &str, age_secs: i64) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    s.with_backend_db(|conn| {
        // Force-steal regardless of any existing live claim -- a
        // sufficiently-future `now` always looks stale to the acquire
        // gate -- then re-acquire as `owner` at the real target
        // timestamp: re-acquiring your own row is unconditionally
        // allowed, so this second call is what actually pins
        // `heartbeat_at` to `age_secs` in the past.
        agentflare_backend::claim::acquire(conn, item_id, owner, now + 14_400 + 1, 14_400)
            .unwrap();
        agentflare_backend::claim::acquire(conn, item_id, owner, now - age_secs, 14_400).unwrap()
    })
    .unwrap();
}

#[test]
fn item_release_errors_when_a_different_owner_holds_a_live_claim() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    seed_claim(&s, &item_id, "someone-else:1", 0);

    let err = s
        .item(Parameters(ItemRequest {
            action: "release".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        s.with_backend_db(|conn| agentflare_backend::claim::current_owner(conn, &item_id))
            .unwrap(),
        Some("someone-else:1".to_string()),
        "a live claim held by someone else must be left untouched"
    );
}

#[test]
fn item_release_reclaims_and_releases_a_stale_claim_from_an_abandoned_owner() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    // Well past the 4h default TTL -- the claiming instance is gone.
    seed_claim(&s, &item_id, "gone:1", 20_000);

    let released: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "release".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(released["released"], true, "{released:?}");
    assert_eq!(
        s.with_backend_db(|conn| agentflare_backend::claim::current_owner(conn, &item_id))
            .unwrap(),
        None,
        "a release from a non-owning caller must clear an abandoned claim, not silently no-op"
    );
}

#[test]
fn item_done_errors_when_a_different_owner_holds_a_live_claim() {
    let (_tmp, s) = harness();
    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    seed_claim(&s, &item_id, "someone-else:1", 0);

    let err = s
        .item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert_eq!(
        s.with_backend_db(|conn| agentflare_backend::claim::current_owner(conn, &item_id))
            .unwrap(),
        Some("someone-else:1".to_string()),
        "a live claim held by someone else must be left untouched"
    );
}

#[test]
fn item_done_recovers_committed_work_left_behind_by_a_stale_claim() {
    // Recurrence of #67/#80: the original claiming instance vanished
    // (crash, session end, daemon restart) with real committed work
    // sitting in its worktree. A fresh caller's `done` must be able to
    // pick that work up and finish it, instead of silently no-op'ing
    // forever because the claim's owner no longer matches anyone alive.
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().to_path_buf();
    let run_git = |dir: &std::path::Path, args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap()
    };
    run_git(&repo_root, &["init", "-b", "master"]);
    run_git(&repo_root, &["config", "user.email", "test@test.com"]);
    run_git(&repo_root, &["config", "user.name", "Test"]);
    run_git(&repo_root, &["commit", "--allow-empty", "-m", "initial"]);

    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root.clone()),
        ..Default::default()
    };

    let created: serde_json::Value =
        serde_json::from_str(&s.item(Parameters(empty_item_create("Test"))).unwrap()).unwrap();
    let item_id = created["id"].as_str().unwrap().to_string();
    let sequence_id = created["sequence_id"].as_i64().unwrap();

    let claimed: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "claim".into(),
            id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let worktree_path = std::path::PathBuf::from(claimed["worktree_path"].as_str().unwrap());
    let branch = format!("task/{sequence_id}");
    std::fs::write(worktree_path.join("real_work.txt"), "already committed").unwrap();
    run_git(&worktree_path, &["add", "real_work.txt"]);
    run_git(&worktree_path, &["commit", "-m", "real committed work"]);

    // Simulate the claiming instance vanishing: the lease now belongs to
    // an owner nobody in this process is, well past the TTL.
    seed_claim(&s, &item_id, "gone:1", 20_000);
    assert_eq!(
        s.with_backend_db(|conn| agentflare_backend::claim::current_owner(conn, &item_id))
            .unwrap(),
        Some("gone:1".to_string())
    );

    // push: false -- no remote configured in this throwaway repo.
    let result: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id.clone()),
            push: Some(false),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(result["done"], true, "{result:?}");
    assert_eq!(result["status"], "completed");

    let log = run_git(
        &repo_root,
        &["log", &branch, "--name-only", "--pretty=format:"],
    );
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("real_work.txt"),
        "the abandoned claim's real committed work must not be lost"
    );
}
