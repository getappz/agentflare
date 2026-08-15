#[allow(unused_imports)]
use super::*;

// Item #109: a real commit landed on the claimed branch and `git push`
// itself succeeded, but no PR resulted (here: `origin` is a local bare
// repo, so `RepoId::resolve_from_remote` can't recognize it as a GitHub
// remote and `push_and_open_pr` skips PR creation -- the same soft-fail
// path a missing `gh` credential or a `gh pr create` error would take).
// `done` must not fall through to `mark_completed`: a pushed branch with
// no PR anywhere is not "in_review", and it's certainly not "completed".
#[test]
fn item_done_reports_a_hard_error_when_push_succeeds_but_no_pr_results() {
    let tmp = tempfile::tempdir().unwrap();
    let repo_dir = tempfile::tempdir().unwrap();
    let repo_root = repo_dir.path().to_path_buf();
    let origin_dir = tempfile::tempdir().unwrap();
    let run_git = |dir: &std::path::Path, args: &[&str]| {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    };
    run_git(origin_dir.path(), &["init", "--bare", "-b", "master"]);
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
            origin_dir.path().to_str().unwrap(),
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
    std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();

    let err = s
        .item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id.clone()),
            summary: Some("Did the real work.".into()),
            ..Default::default()
        }))
        .unwrap_err();

    assert!(
        err.message.contains("no PR resulted") || err.message.contains("not marking completed"),
        "error must call out the missing PR: {}",
        err.message
    );

    let comments: serde_json::Value = serde_json::from_str(
        &s.comment(Parameters(CommentRequest {
            action: "list".into(),
            item_id: Some(item_id.clone()),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    let arr = comments.as_array().unwrap();
    assert!(
        arr.iter().any(|c| c["body"]
            .as_str()
            .unwrap_or_default()
            .contains("PR creation failed")),
        "a PR-failure comment must be posted on the item: {arr:?}"
    );

    let item_state: serde_json::Value = serde_json::from_str(
        &s.item(Parameters(ItemRequest {
            action: "get".into(),
            id: Some(item_id),
            ..Default::default()
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(
        item_state["completed_at"].is_null(),
        "a push with no resulting PR must not be treated as completion: {item_state:?}"
    );
}
