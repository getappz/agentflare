//! `rebase_item_worktree` on a branch that has already been pushed: once a
//! PR exists, GitHub's update-branch (the supervisor's `pulls::update_branch`)
//! owns base drift, so the local worktree must follow `origin/<branch>`
//! instead of rewriting it (item #331).

use super::tests::{init_remote_and_local_clone, test_item};
use super::*;
use std::path::PathBuf;

fn sha(path: &Path, rev: &str) -> String {
    run_git_in(path, &["rev-parse", rev]).unwrap()
}

fn commit(path: &Path, msg: &str) {
    run_git_in(path, &["commit", "--allow-empty", "-m", msg]).unwrap();
}

/// A claimed worktree with one commit of its own, pushed to `origin`.
/// Returns the worktree path, its branch name and the pushed commit.
fn pushed_worktree(local: &Path) -> (PathBuf, String, String) {
    let item = test_item(1);
    let worktree = create_worktree(&item, local, "master", None).unwrap();
    std::fs::write(worktree.join("work.txt"), b"in progress").unwrap();
    run_git_in(&worktree, &["add", "work.txt"]).unwrap();
    run_git_in(&worktree, &["commit", "-m", "in-progress work"]).unwrap();
    let branch = run_git_in(&worktree, &["symbolic-ref", "--short", "HEAD"]).unwrap();
    run_git_in(&worktree, &["push", "-u", "origin", &branch]).unwrap();
    let work = sha(&worktree, "HEAD");
    (worktree, branch, work)
}

/// What GitHub's "update branch" does: a merge of the base into the PR
/// branch, on the remote only.
fn remote_merges_master_into(remote: &Path, branch: &str) {
    commit(remote, "merged elsewhere");
    run_git_in(remote, &["switch", branch]).unwrap();
    run_git_in(
        remote,
        &[
            "merge",
            "--no-ff",
            "master",
            "-m",
            "Merge branch 'master' into the PR branch",
        ],
    )
    .unwrap();
    run_git_in(remote, &["switch", "master"]).unwrap();
}

#[test]
fn pushed_branch_follows_the_remotes_update_branch_merge_instead_of_rebasing() {
    let (remote, _container, local) = init_remote_and_local_clone();
    let (worktree, branch, work) = pushed_worktree(&local);
    remote_merges_master_into(&remote.path, &branch);
    let remote_tip = sha(&remote.path, &branch);

    let outcome = rebase_item_worktree(&test_item(1), &local, "master");

    assert!(
        matches!(outcome, RebaseOutcome::FollowedRemote),
        "expected FollowedRemote"
    );
    assert_eq!(
        sha(&worktree, "HEAD"),
        remote_tip,
        "fast-forwarded to the remote branch"
    );
    assert!(
        run_git_in_ok(&worktree, &["merge-base", "--is-ancestor", &work, "HEAD"]),
        "the pushed commit keeps its sha: history was not rewritten"
    );
}

#[test]
fn pushed_branch_is_left_alone_when_only_the_target_moved() {
    let (remote, _container, local) = init_remote_and_local_clone();
    let (worktree, _branch, work) = pushed_worktree(&local);
    commit(&remote.path, "merged elsewhere");

    let outcome = rebase_item_worktree(&test_item(1), &local, "master");

    assert!(
        matches!(outcome, RebaseOutcome::UpToDate),
        "a pushed branch is never rebased; base drift is GitHub's job"
    );
    assert_eq!(sha(&worktree, "HEAD"), work);
}

#[test]
fn pushed_branch_with_commits_on_both_sides_is_left_alone() {
    let (remote, _container, local) = init_remote_and_local_clone();
    let (worktree, branch, _work) = pushed_worktree(&local);
    commit(&worktree, "more local work");
    let local_tip = sha(&worktree, "HEAD");
    run_git_in(&remote.path, &["switch", &branch]).unwrap();
    commit(&remote.path, "human fixup on the remote");
    run_git_in(&remote.path, &["switch", "master"]).unwrap();

    let outcome = rebase_item_worktree(&test_item(1), &local, "master");

    assert!(
        matches!(outcome, RebaseOutcome::Diverged),
        "expected Diverged"
    );
    assert_eq!(
        sha(&worktree, "HEAD"),
        local_tip,
        "nothing was merged or rewritten"
    );
}

#[test]
fn never_pushed_branch_still_rebases_onto_the_latest_target() {
    let (remote, _container, local) = init_remote_and_local_clone();
    let item = test_item(1);
    let worktree = create_worktree(&item, &local, "master", None).unwrap();
    commit(&worktree, "in-progress work");
    commit(&remote.path, "merged elsewhere");
    let remote_head = sha(&remote.path, "HEAD");

    let outcome = rebase_item_worktree(&item, &local, "master");

    assert!(
        matches!(outcome, RebaseOutcome::Rebased),
        "expected Rebased"
    );
    assert!(run_git_in_ok(
        &worktree,
        &["merge-base", "--is-ancestor", &remote_head, "HEAD"]
    ));
}
