//! Self-heal / adopt / lock / push-integration tests for `worktree`,
//! split out of `worktree_tests.rs` for size. Shares its repo/item fixtures.

use super::tests::{init_remote_and_local_clone, init_repo, test_item};
use super::*;
use std::time::Duration;
use tempfile::TempDir;

#[test]
fn create_worktree_switches_a_detached_checkout_back_onto_its_branch() {
    // An agent's `git checkout <sha>` (or an interrupted rebase) leaves the
    // task worktree detached; `worktree add` then refused the occupied path
    // on every redispatch, forever.
    let repo = init_repo();
    let item = test_item(1);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    std::fs::write(wt.join("a.txt"), "a").unwrap();
    run_git_in(&wt, &["add", "a.txt"]).unwrap();
    run_git_in(&wt, &["commit", "-m", "a"]).unwrap();
    run_git_in(&wt, &["checkout", "--detach", "HEAD~1"]).unwrap();

    let again = create_worktree(&item, &repo.path, "master", None).unwrap();
    assert_eq!(again, wt);
    assert_eq!(
        run_git_in(&wt, &["branch", "--show-current"]).unwrap(),
        task_branch_name(&item)
    );
    assert!(
        wt.join("a.txt").exists(),
        "the branch's own commit must be checked out"
    );
}

#[test]
fn create_worktree_fast_forwards_the_branch_over_detached_commits() {
    let repo = init_repo();
    let item = test_item(1);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    run_git_in(&wt, &["checkout", "--detach"]).unwrap();
    std::fs::write(wt.join("detached.txt"), "d").unwrap();
    run_git_in(&wt, &["add", "detached.txt"]).unwrap();
    run_git_in(&wt, &["commit", "-m", "made while detached"]).unwrap();
    let detached_head = run_git_in(&wt, &["rev-parse", "HEAD"]).unwrap();

    create_worktree(&item, &repo.path, "master", None).unwrap();
    assert_eq!(
        run_git_in(&wt, &["rev-parse", &task_branch_name(&item)]).unwrap(),
        detached_head,
        "work committed while detached must end up on the task branch"
    );
}

#[test]
fn create_worktree_rescues_diverged_detached_commits_before_switching() {
    let repo = init_repo();
    let item = test_item(1);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    std::fs::write(wt.join("on_branch.txt"), "b").unwrap();
    run_git_in(&wt, &["add", "on_branch.txt"]).unwrap();
    run_git_in(&wt, &["commit", "-m", "on branch"]).unwrap();
    run_git_in(&wt, &["checkout", "--detach", "HEAD~1"]).unwrap();
    std::fs::write(wt.join("stray.txt"), "s").unwrap();
    run_git_in(&wt, &["add", "stray.txt"]).unwrap();
    run_git_in(&wt, &["commit", "-m", "stray detached work"]).unwrap();
    let stray = run_git_in(&wt, &["rev-parse", "HEAD"]).unwrap();

    create_worktree(&item, &repo.path, "master", None).unwrap();
    let rescued = run_git_in(
        &repo.path,
        &[
            "for-each-ref",
            "--format=%(objectname)",
            "refs/agentflare/rescue/",
        ],
    )
    .unwrap();
    assert!(
        rescued.contains(&stray),
        "diverged detached commit must be pinned: {rescued}"
    );
}

#[test]
fn create_worktree_recovers_a_checkout_left_mid_rebase_with_a_stale_lock() {
    let (remote, _c, local) = init_remote_and_local_clone();
    std::fs::write(remote.path.join("shared.txt"), "base\n").unwrap();
    run_git_in(&remote.path, &["add", "shared.txt"]).unwrap();
    run_git_in(&remote.path, &["commit", "-m", "base"]).unwrap();
    run_git_in(&local, &["pull"]).unwrap();
    let item = test_item(1);
    let wt = create_worktree(&item, &local, "master", None).unwrap();
    std::fs::write(wt.join("shared.txt"), "mine\n").unwrap();
    run_git_in(&wt, &["commit", "-am", "mine"]).unwrap();
    std::fs::write(remote.path.join("shared.txt"), "theirs\n").unwrap();
    run_git_in(&remote.path, &["commit", "-am", "theirs"]).unwrap();
    run_git_in(&wt, &["fetch", "origin"]).unwrap();
    // Simulate a rebase killed mid-conflict: rebase state + abandoned lock.
    let _ = run_git_in(&wt, &["rebase", "origin/master"]);
    let lock = worktree_git_path(&wt, "index.lock").unwrap();
    std::fs::write(&lock, "").unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(old)
        .unwrap();

    create_worktree(&item, &local, "master", None).unwrap();
    assert!(!lock.exists(), "abandoned index.lock must be cleared");
    assert!(!worktree_git_path(&wt, "rebase-merge").unwrap().exists());
    assert_eq!(
        run_git_in(&wt, &["branch", "--show-current"]).unwrap(),
        task_branch_name(&item)
    );
}

#[test]
fn heal_leaves_a_fresh_index_lock_alone() {
    let repo = init_repo();
    let item = test_item(1);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    let lock = worktree_git_path(&wt, "index.lock").unwrap();
    std::fs::write(&lock, "").unwrap();
    heal_interrupted_git_state(&wt, false).unwrap();
    assert!(
        lock.exists(),
        "a lock a live git may still hold must not be removed"
    );
}

#[test]
fn create_worktree_recreates_a_half_created_checkout() {
    let repo = init_repo();
    let item = test_item(1);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    // `worktree add` killed while populating: its "initializing" lock stays.
    let locked = worktree_git_path(&wt, "locked").unwrap();
    std::fs::write(&locked, "initializing").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&locked)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(600))
        .unwrap();
    std::fs::remove_file(wt.join("README.md")).ok();

    create_worktree(&item, &repo.path, "master", None).unwrap();
    assert!(
        !std::fs::read_to_string(&locked)
            .unwrap_or_default()
            .contains("initializing"),
        "the half-created registration must be replaced"
    );
    let status = run_git_in(&wt, &["status", "--porcelain"]).unwrap();
    assert!(
        status.is_empty(),
        "recreated checkout must be fully populated: {status}"
    );
}

#[test]
fn create_worktree_clears_a_directory_with_a_broken_git_pointer() {
    let repo = init_repo();
    let item = test_item(1);
    let wt = item_worktree_path(&repo.path, item.sequence_id);
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), "gitdir: /nonexistent/admin\n").unwrap();
    std::fs::write(wt.join("leftover.txt"), "x").unwrap();
    let main_branch_before = run_git_in(&repo.path, &["branch", "--show-current"]).unwrap();

    create_worktree(&item, &repo.path, "master", None).unwrap();
    assert_eq!(
        run_git_in(&wt, &["branch", "--show-current"]).unwrap(),
        task_branch_name(&item)
    );
    assert_eq!(
        run_git_in(&repo.path, &["branch", "--show-current"]).unwrap(),
        main_branch_before,
        "the main checkout must never be touched"
    );
}

#[test]
fn commit_uncommitted_at_refuses_a_directory_that_is_not_its_own_checkout() {
    let repo = init_repo();
    let wt = item_worktree_path(&repo.path, 1);
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join("x.txt"), "x").unwrap();
    let head_before = run_git_in(&repo.path, &["rev-parse", "HEAD"]).unwrap();
    assert!(matches!(
        commit_uncommitted_at(&wt, "m", true),
        CommitOutcome::Failed(_)
    ));
    assert_eq!(
        run_git_in(&repo.path, &["rev-parse", "HEAD"]).unwrap(),
        head_before,
        "must not commit into the enclosing main repository"
    );
}

#[test]
fn cleanup_item_worktree_pins_detached_commits_before_removing() {
    let repo = init_repo();
    let item = test_item(1);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    run_git_in(&wt, &["checkout", "--detach"]).unwrap();
    run_git_in(&wt, &["commit", "--allow-empty", "-m", "detached only"]).unwrap();
    let head = run_git_in(&wt, &["rev-parse", "HEAD"]).unwrap();
    assert!(cleanup_item_worktree(&item, &repo.path));
    assert!(
        run_git_in_ok(
            &repo.path,
            &["cat-file", "-e", &format!("{head}^{{commit}}")]
        ) && !run_git_in(
            &repo.path,
            &[
                "for-each-ref",
                "--contains",
                &head,
                "refs/agentflare/rescue/"
            ]
        )
        .unwrap()
        .is_empty(),
        "detached-only commit must stay reachable after cleanup"
    );
}

#[test]
fn gc_orphans_does_not_drop_another_worktrees_registration() {
    let repo = init_repo();
    let a = test_item(1);
    let b = test_item(2);
    create_worktree(&a, &repo.path, "master", None).unwrap();
    let wt_b = create_worktree(&b, &repo.path, "master", None).unwrap();
    // B's admin entry looks stale (e.g. its directory was moved and back).
    let moved = repo.path.join("moved-b");
    std::fs::rename(&wt_b, &moved).unwrap();
    assert!(cleanup_item_worktree(&a, &repo.path));
    std::fs::rename(&moved, &wt_b).unwrap();
    assert_eq!(
        run_git_in(&wt_b, &["branch", "--show-current"]).unwrap(),
        task_branch_name(&b),
        "B's registration must survive A's cleanup"
    );
}

#[test]
fn branch_diverged_ignores_upstream_commits_missing_from_a_stale_local_target() {
    let (remote, _c, local) = init_remote_and_local_clone();
    let item = test_item(1);
    create_worktree(&item, &local, "master", None).unwrap();
    // Upstream moves on; the local `master` is never pulled.
    run_git_in(&remote.path, &["commit", "--allow-empty", "-m", "upstream"]).unwrap();
    rebase_item_worktree(&item, &local, "master");
    let branch = resolve_item_task_branch(&item, &local);
    assert!(
        !branch_diverged(&local, &branch, "master"),
        "a branch with no commits of its own must not read as diverged"
    );
}

#[test]
fn squash_since_refuses_a_base_that_is_no_longer_an_ancestor() {
    let repo = init_repo();
    let item = test_item(1);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    run_git_in(&wt, &["commit", "--allow-empty", "-m", "one"]).unwrap();
    let base = head_sha(&wt).unwrap();
    run_git_in(&wt, &["reset", "--hard", "HEAD~1"]).unwrap();
    run_git_in(&wt, &["commit", "--allow-empty", "-m", "rewritten"]).unwrap();
    assert!(squash_since(&wt, &base).is_err());
}

#[test]
fn push_branch_integrates_commits_pushed_to_the_branch_by_someone_else() {
    let (remote, _c, local) = init_remote_and_local_clone();
    let item = test_item(1);
    let wt = create_worktree(&item, &local, "master", None).unwrap();
    std::fs::write(wt.join("mine.txt"), "m").unwrap();
    run_git_in(&wt, &["add", "mine.txt"]).unwrap();
    run_git_in(&wt, &["commit", "-m", "mine"]).unwrap();
    let branch = push_branch(&item, &local, "master", None).expect("first push");

    // A human pushes a fixup to the PR branch; then any fetch in the repo
    // advances origin/<branch>, which a bare lease would trust.
    let other = TempDir::new().unwrap();
    let other_path = other.path().join("o");
    run_git_in(
        other.path(),
        &[
            "clone",
            "-b",
            &branch,
            remote.path.to_str().unwrap(),
            other_path.to_str().unwrap(),
        ],
    )
    .unwrap();
    run_git_in(&other_path, &["config", "user.email", "h@h"]).unwrap();
    run_git_in(&other_path, &["config", "user.name", "H"]).unwrap();
    std::fs::write(other_path.join("human.txt"), "h").unwrap();
    run_git_in(&other_path, &["add", "human.txt"]).unwrap();
    run_git_in(&other_path, &["commit", "-m", "human fixup"]).unwrap();
    run_git_in(&other_path, &["push", "origin", &branch]).unwrap();
    let human = run_git_in(&other_path, &["rev-parse", "HEAD"]).unwrap();
    run_git_in(&local, &["fetch", "origin"]).unwrap();

    std::fs::write(wt.join("more.txt"), "more").unwrap();
    run_git_in(&wt, &["add", "more.txt"]).unwrap();
    run_git_in(&wt, &["commit", "-m", "more"]).unwrap();
    assert!(push_branch(&item, &local, "master", None).is_some());
    let tip = run_git_in(&remote.path, &["rev-parse", &branch]).unwrap();
    assert!(
        run_git_in(&remote.path, &["log", "--format=%s", &tip])
            .unwrap()
            .contains("human fixup"),
        "the human's commit ({human}) must survive the agent's push"
    );
}

#[test]
fn push_branch_integration_does_not_replay_new_target_commits_into_the_pr_branch() {
    // Rebasing onto a newer target and then *rebasing* onto origin/<branch>
    // replayed the target's upstream commits as fresh copies, so they showed
    // up as the item's own commits in its PR. Merging keeps them as-is.
    let (remote, _c, local) = init_remote_and_local_clone();
    let item = test_item(1);
    let wt = create_worktree(&item, &local, "master", None).unwrap();
    std::fs::write(wt.join("mine.txt"), "m").unwrap();
    run_git_in(&wt, &["add", "mine.txt"]).unwrap();
    run_git_in(&wt, &["commit", "-m", "mine"]).unwrap();
    let branch = push_branch(&item, &local, "master", None).expect("first push");

    let other = TempDir::new().unwrap();
    let other_path = other.path().join("o");
    run_git_in(
        other.path(),
        &[
            "clone",
            "-b",
            &branch,
            remote.path.to_str().unwrap(),
            other_path.to_str().unwrap(),
        ],
    )
    .unwrap();
    run_git_in(&other_path, &["config", "user.email", "h@h"]).unwrap();
    run_git_in(&other_path, &["config", "user.name", "H"]).unwrap();
    std::fs::write(other_path.join("human.txt"), "h").unwrap();
    run_git_in(&other_path, &["add", "human.txt"]).unwrap();
    run_git_in(&other_path, &["commit", "-m", "human fixup"]).unwrap();
    run_git_in(&other_path, &["push", "origin", &branch]).unwrap();

    // Meanwhile the target advances with an unrelated upstream commit.
    std::fs::write(remote.path.join("upstream.txt"), "u").unwrap();
    run_git_in(&remote.path, &["add", "upstream.txt"]).unwrap();
    run_git_in(&remote.path, &["commit", "-m", "upstream only"]).unwrap();

    std::fs::write(wt.join("more.txt"), "more").unwrap();
    run_git_in(&wt, &["add", "more.txt"]).unwrap();
    run_git_in(&wt, &["commit", "-m", "more"]).unwrap();
    assert!(push_branch(&item, &local, "master", None).is_some());

    let range = format!("master..{branch}");
    let pr_log = run_git_in(&remote.path, &["log", "--format=%s", &range]).unwrap();
    assert!(
        !pr_log.contains("upstream only"),
        "an upstream-only target commit must not appear as a PR commit: {pr_log}"
    );
    assert!(pr_log.contains("human fixup"), "{pr_log}");
    assert!(pr_log.contains("more"), "{pr_log}");
    let files = run_git_in(
        &remote.path,
        &["diff", "--name-only", &format!("master...{branch}")],
    )
    .unwrap();
    let mut files: Vec<&str> = files.lines().collect();
    files.sort_unstable();
    assert_eq!(files, ["human.txt", "mine.txt", "more.txt"]);
}

#[test]
fn retryable_worktree_race_matches_real_git_lock_messages() {
    assert!(is_retryable_worktree_race(
        "fatal: cannot lock ref 'refs/heads/task/1': Unable to create '/r/.git/refs/heads/task/1.lock': File exists."
    ));
    assert!(is_retryable_worktree_race(
        "fatal: Unable to create '/r/.git/index.lock': File exists."
    ));
}

#[test]
fn create_worktree_locks_the_worktree_and_cleanup_still_removes_it() {
    let repo = init_repo();
    let item = test_item(1);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    let locked = worktree_git_path(&wt, "locked").unwrap();
    assert!(
        locked.exists(),
        "a claimed worktree must be locked against outside prunes"
    );
    run_git_in(&repo.path, &["worktree", "prune"]).unwrap();
    assert!(cleanup_item_worktree(&item, &repo.path));
    let list = run_git_in(&repo.path, &["worktree", "list", "--porcelain"]).unwrap();
    assert!(
        !list.contains(".worktrees/task/1"),
        "registration must be gone too: {list}"
    );
    // And the item can be claimed again afterwards.
    create_worktree(&item, &repo.path, "master", None).unwrap();
}

#[test]
fn create_worktree_recovers_when_its_own_locked_worktree_dir_vanished() {
    let repo = init_repo();
    let item = test_item(1);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    std::fs::remove_dir_all(&wt).unwrap();
    create_worktree(&item, &repo.path, "master", None)
        .expect("our own lock must not wedge a re-claim after the directory was removed");
}

#[test]
fn git_children_get_english_non_interactive_env() {
    let mut cmd = std::process::Command::new("true");
    crate::shell::apply_filtered_path(&mut cmd);
    let envs: std::collections::HashMap<_, _> = cmd
        .get_envs()
        .map(|(k, v)| {
            (
                k.to_string_lossy().to_string(),
                v.map(|v| v.to_string_lossy().to_string()),
            )
        })
        .collect();
    assert_eq!(envs.get("LC_MESSAGES"), Some(&Some("C".to_string())));
    assert_eq!(
        envs.get("LC_ALL"),
        Some(&None),
        "LC_ALL must be removed so LC_MESSAGES applies"
    );
    assert_eq!(
        envs.get("GIT_TERMINAL_PROMPT"),
        Some(&Some("0".to_string()))
    );
    assert_eq!(envs.get("GIT_OPTIONAL_LOCKS"), Some(&Some("0".to_string())));
}

#[test]
fn same_location_matches_a_deleted_dir_through_its_canonical_parent() {
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("task");
    std::fs::create_dir_all(&real).unwrap();
    // A differently-spelled path to the same (existing) parent, like a
    // Windows 8.3 short name or a symlinked temp dir.
    let alias = tmp.path().join("task").join("..").join("task").join("1");
    let gone = real.join("1");
    assert!(same_location(&alias, &gone));
    assert!(!same_location(&real.join("2"), &gone));
}
