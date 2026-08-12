#[cfg(unix)]
use super::*;

// Windows' FILE_ATTRIBUTE_READONLY on a *directory* doesn't prevent writes
// to files inside it (unlike Unix permission bits) -- the readonly-git-dir
// simulation below relies on that Unix-specific behavior to make `git add`
// fail deterministically, so on Windows the auto-commit silently succeeds
// instead of failing, and the test's premise doesn't hold there.
#[cfg(unix)]
#[test]
fn item_done_reports_a_hard_error_when_auto_commit_fails_on_a_dirty_tree() {
    // Item #92: unlike the fallback case above (where `status` itself
    // fails and the outcome is indistinguishable from a genuine no-op),
    // here `status` confirms there IS something to commit but `add`/
    // `commit` fails anyway -- e.g. item #88's read-only `.git` under
    // bwrap. `done` must report this loudly (an error, plus a comment on
    // the item) rather than silently folding it into
    // `nothing_was_ever_committed`'s release-and-cleanup path, which would
    // strand the real edits while reporting success.
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
    std::fs::write(worktree_path.join("real_work.txt"), "real work").unwrap();

    // `status` must still succeed (so it reports a dirty tree), but `add`
    // must fail -- make the real git dir unwritable, distinct from the
    // "gitdir pointer is entirely broken" fallback test above.
    let gitdir_pointer = std::fs::read_to_string(worktree_path.join(".git")).unwrap();
    let git_dir = std::path::PathBuf::from(gitdir_pointer.trim().strip_prefix("gitdir: ").unwrap());
    let original_perms = std::fs::metadata(&git_dir).unwrap().permissions();
    let mut perms = original_perms.clone();
    perms.set_readonly(true);
    std::fs::set_permissions(&git_dir, perms).unwrap();

    let err = s
        .item(Parameters(ItemRequest {
            action: "done".into(),
            id: Some(item_id.clone()),
            summary: Some("Did the real work.".into()),
            push: Some(false),
            ..Default::default()
        }))
        .unwrap_err();

    // Restore the original permissions (not a blanket writable toggle,
    // which on Unix would leave the dir world-writable) so the temp dir
    // can be cleaned up.
    std::fs::set_permissions(&git_dir, original_perms).unwrap();

    assert!(
        err.message.contains("auto-commit"),
        "error must call out the auto-commit failure: {}",
        err.message
    );
    assert!(
        worktree_path.join("real_work.txt").exists(),
        "the uncommitted real work must be left in place, not lost"
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
            .contains("commit failed")),
        "a commit-failure comment must be posted on the item: {arr:?}"
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
        "a failed auto-commit must not be treated as completion: {item_state:?}"
    );
}
