use super::*;

#[test]
fn item_doctor_scans_without_reclaiming_by_default() {
    // Item #465: `agentflare git doctor` (item #235) was CLI-only -- an
    // MCP-only agent hitting the `git worktree remove/prune` shim denial
    // had no reachable way to run it. `reclaim` defaults to false, matching
    // the CLI's own `--reclaim` opt-in flag.
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

    let report: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "doctor".into(),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(report["lanes"].as_array().unwrap().len(), 1, "{report:?}");
    assert_eq!(
        report["reclaimed"].as_array().unwrap().len(),
        0,
        "reclaim defaults to false — nothing must be deleted: {report:?}"
    );
}

#[test]
fn item_doctor_reclaims_a_stale_clean_linked_worktree() {
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

    let linked_path = repo_dir.path().join("linked-wt");
    run_git(
        &repo_root,
        &[
            "worktree",
            "add",
            "-b",
            "linked-branch",
            linked_path.to_str().unwrap(),
        ],
    );
    assert!(linked_path.exists());

    let s = AgentflareMcp {
        backend_db_override: Some(tmp.path().join("backend.db")),
        backend_project_link_override: Some(tmp.path().join("project.json")),
        worktree_repo_root_override: Some(repo_root),
        ..Default::default()
    };

    let report: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "doctor".into(),
            // 0 days makes every lane immediately stale, so the clean
            // linked worktree is reclaim-eligible without needing to fake
            // an old commit timestamp.
            staleness_days: Some(0),
            reclaim: Some(true),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let reclaimed = report["reclaimed"].as_array().unwrap();
    assert_eq!(
        reclaimed,
        &vec![serde_json::json!("linked-branch")],
        "{report:?}"
    );
    assert!(
        !linked_path.exists(),
        "reclaim=true must delete a clean stale linked worktree"
    );
}
