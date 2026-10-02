use super::*;

#[test]
fn item_release_preserve_worktree_leaves_a_clean_worktree() {
    // Item #322: orphan-restart reconcile releases the claim so rediscovery
    // can reclaim, but must NOT delete a clean mid-work checkout — the next
    // dispatch resumes that same `.worktrees/task/<id>`.
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
    // Committed mid-work: clean tree, but the checkout is still live work.
    std::fs::write(worktree_path.join("mid_work.txt"), "committed but not PR'd").unwrap();
    let run_wt = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(&worktree_path)
            .output()
            .unwrap()
    };
    run_wt(&["add", "mid_work.txt"]);
    run_wt(&["commit", "-m", "mid-work"]);

    let released: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "release".into(),
            id: Some(item_id),
            preserve_worktree: Some(true),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(released["released"], true, "{released:?}");
    assert!(
        worktree_path.exists(),
        "preserve_worktree release must leave a clean mid-work checkout in place"
    );
    assert!(
        worktree_path.join("mid_work.txt").exists(),
        "committed mid-work must still be present after preserve_worktree release"
    );
}
