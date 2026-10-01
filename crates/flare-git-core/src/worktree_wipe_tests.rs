//! Regression tests for the claimed-worktree wipe (item #689): a live
//! worktree was garbage-collected because one git call inside it failed,
//! and a removal that hit a locked file left it hollowed out. Split out of
//! `worktree_tests.rs` for size. Shares its repo/item fixtures.

use super::tests::{init_repo, test_item};
use super::*;
use crate::shell::test_support::Repo;

/// A repo whose `master` tracks `n` files, so a wipe is measurable.
fn seeded_repo(n: usize) -> Repo {
    let repo = init_repo();
    for i in 0..n {
        std::fs::write(repo.path.join(format!("f{i}.txt")), "tracked\n").unwrap();
    }
    run_git_in(&repo.path, &["add", "-A"]).unwrap();
    run_git_in(&repo.path, &["commit", "-m", "seed"]).unwrap();
    repo
}

fn tracked_files_present(worktree: &Path, n: usize) -> usize {
    (0..n)
        .filter(|i| worktree.join(format!("f{i}.txt")).exists())
        .count()
}

// The original failure path: the daemon inherited a `GIT_WORK_TREE` naming
// the main repo, so `git rev-parse --show-toplevel` inside every task
// worktree answered for the main repo, `is_own_checkout` read that as "not
// a checkout", and `create_worktree` garbage-collected the claimant's own
// live worktree on the next dispatch. The env has to be really leaked into
// the process, so this re-execs the test binary (same shape as
// `shell::tests::run_in_survives_a_path_bloated_by_repeated_daemon_dispatches`).
#[test]
fn create_worktree_keeps_a_live_worktree_when_git_work_tree_leaks() {
    const MARKER: &str = "WORKTREE_LEAKED_GIT_ENV_CHILD";
    const TEST_NAME: &str =
        "worktree::wipe_tests::create_worktree_keeps_a_live_worktree_when_git_work_tree_leaks";
    let item = test_item(686);

    if let Some(repo) = std::env::var_os(MARKER) {
        let repo = PathBuf::from(repo);
        let wt = item_worktree_path(&repo, item.sequence_id);
        let healthy = is_own_checkout(&wt);
        let reclaimed = create_worktree(&item, &repo, "master", None);
        println!("child: is_own_checkout={healthy} create_worktree={reclaimed:?}");
        std::process::exit(if healthy && reclaimed.is_ok() { 0 } else { 1 });
    }

    let repo = seeded_repo(30);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    std::fs::write(wt.join("precious.txt"), "uncommitted work").unwrap();

    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([TEST_NAME, "--exact", "--nocapture"])
        .env(MARKER, &repo.path)
        .env("GIT_WORK_TREE", &repo.path)
        .output()
        .expect("failed to re-exec test binary");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("running 1 test"),
        "expected --exact to select exactly this test, got:\n{stdout}"
    );
    assert!(
        wt.join("precious.txt").exists(),
        "a leaked GIT_WORK_TREE got the claimant's live worktree wiped; child output:\n{stdout}"
    );
    assert_eq!(
        tracked_files_present(&wt, 30),
        30,
        "child output:\n{stdout}"
    );
    assert!(
        output.status.success(),
        "a healthy worktree must still read as its own checkout and be reused under a leaked \
         GIT_WORK_TREE; child output:\n{stdout}"
    );
}

// F1: "git failed inside it" is not "this directory is not a checkout".
// The `.git` pointer and its admin entry are intact here; only the admin
// `HEAD` is unreadable, so every git command in the worktree errors.
#[test]
fn create_worktree_never_clears_a_structurally_intact_worktree() {
    let repo = seeded_repo(30);
    let item = test_item(686);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    std::fs::write(wt.join("precious.txt"), "uncommitted work").unwrap();
    let admin = repo.path.join(".git").join("worktrees").join("686");
    std::fs::remove_file(admin.join("HEAD")).unwrap();
    assert!(
        !is_own_checkout(&wt),
        "precondition: git no longer answers inside the worktree"
    );

    let err = create_worktree(&item, &repo.path, "master", None)
        .expect_err("an intact worktree git cannot answer for must fail the claim, not be cleared");
    assert!(err.contains("nothing was deleted"), "{err}");
    assert!(
        wt.join("precious.txt").exists(),
        "uncommitted work destroyed"
    );
    assert_eq!(tracked_files_present(&wt, 30), 30);
    assert!(admin.is_dir(), "registration must be left alone");
}

// F3: when the directory cannot be moved aside, nothing may be deleted --
// a partial `remove_dir_all` is what hollowed the worktree out.
#[test]
fn remove_worktree_dir_deletes_nothing_when_it_cannot_move_the_directory_aside() {
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("wt");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), b"a").unwrap();
    std::fs::write(dir.join("sub").join("b.txt"), b"b").unwrap();
    // A plain file where the trash directory would go: the rename has no
    // destination.
    std::fs::write(tmp.path().join(".trash"), b"not a directory").unwrap();

    assert!(!remove_worktree_dir(&dir, "test"));
    assert!(dir.join("a.txt").exists());
    assert!(dir.join("sub").join("b.txt").exists());
}

#[cfg(windows)]
#[test]
fn remove_worktree_dir_leaves_a_locked_directory_fully_intact() {
    use std::os::windows::fs::OpenOptionsExt;
    let tmp = tempfile::TempDir::new().unwrap();
    let dir = tmp.path().join("wt");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), b"a").unwrap();
    std::fs::write(dir.join("sub").join("b.txt"), b"b").unwrap();
    let locked_file = dir.join("sub").join("locked.exe");
    std::fs::write(&locked_file, b"running").unwrap();
    // No sharing at all: what a running test exe under `target\` or
    // another process's handle looks like, held for the whole call.
    let _held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&locked_file)
        .unwrap();

    assert!(
        !remove_worktree_dir(&dir, "test"),
        "a directory with a locked file cannot be removed"
    );
    assert!(dir.join("a.txt").exists(), "unlocked files were deleted");
    assert!(dir.join("sub").join("b.txt").exists());
    assert!(locked_file.exists());
}

// F5: a re-dispatch into a worktree whose tracked files were wiped puts
// them back instead of handing the agent an empty tree to commit.
#[test]
fn create_worktree_restores_a_reclaimed_worktree_whose_tracked_files_were_deleted() {
    let repo = seeded_repo(30);
    let item = test_item(686);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    for i in 0..30 {
        std::fs::remove_file(wt.join(format!("f{i}.txt"))).unwrap();
    }

    create_worktree(&item, &repo.path, "master", None).unwrap();
    assert_eq!(tracked_files_present(&wt, 30), 30);
    let status = run_git_in(&wt, &["status", "--porcelain"]).unwrap();
    assert!(status.is_empty(), "restored tree must be clean: {status}");
}

#[test]
fn create_worktree_fails_a_reclaim_into_a_wiped_worktree_holding_other_changes() {
    let repo = seeded_repo(30);
    let item = test_item(686);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    for i in 1..30 {
        std::fs::remove_file(wt.join(format!("f{i}.txt"))).unwrap();
    }
    std::fs::write(wt.join("f0.txt"), "real edit\n").unwrap();

    let err = create_worktree(&item, &repo.path, "master", None)
        .expect_err("a wipe mixed with real edits must fail the dispatch, not be auto-restored");
    assert!(err.contains("mass deletion"), "{err}");
    assert_eq!(
        std::fs::read_to_string(wt.join("f0.txt")).unwrap(),
        "real edit\n",
        "the uncommitted edit must not be overwritten"
    );
    assert_eq!(tracked_files_present(&wt, 30), 1, "nothing may be restored");
}

// F1: a directory with no `.git` at all is structurally broken, and a
// fresh claim clears it (`create_worktree_clears_a_directory_with_a_broken_
// git_pointer` covers that). The item's live claimant never does.
#[test]
fn create_worktree_never_clears_a_directory_for_its_live_claimant() {
    let repo = init_repo();
    let item = test_item(1);
    let wt = item_worktree_path(&repo.path, item.sequence_id);
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join("leftover.txt"), "x").unwrap();
    assert!(is_structurally_broken(&repo.path, &wt));

    let err = create_worktree_for(&item, &repo.path, "master", None, true).unwrap_err();
    assert!(err.contains("live claim"), "{err}");
    assert!(err.contains("nothing was deleted"), "{err}");
    assert!(wt.join("leftover.txt").exists());

    create_worktree_for(&item, &repo.path, "master", None, false)
        .expect("without a live claim the broken directory is cleared and recreated");
    assert!(is_own_checkout(&wt));
}

#[test]
fn is_structurally_broken_reads_structure_not_git_answers() {
    let repo = init_repo();
    let item = test_item(1);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    assert!(!is_structurally_broken(&repo.path, &wt));

    // Pointer resolves to a real directory that is not a registration.
    let elsewhere = tempfile::TempDir::new().unwrap();
    std::fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", elsewhere.path().display()),
    )
    .unwrap();
    let admin = repo.path.join(".git").join("worktrees").join("1");
    std::fs::write(admin.join("gitdir"), "/nonexistent/elsewhere/.git\n").unwrap();
    std::fs::remove_file(admin.join("locked")).unwrap();
    run_git_in(&repo.path, &["worktree", "prune"]).unwrap();
    assert!(!admin.exists(), "precondition: the registration is gone");
    assert!(
        is_structurally_broken(&repo.path, &wt),
        "an unregistered directory is not a worktree, wherever its pointer leads"
    );

    std::fs::write(wt.join(".git"), "gitdir: /nonexistent/admin\n").unwrap();
    assert!(is_structurally_broken(&repo.path, &wt), "dangling pointer");
    std::fs::remove_file(wt.join(".git")).unwrap();
    assert!(is_structurally_broken(&repo.path, &wt), "missing pointer");
}

// F2: the scrub itself, without needing a leaked process environment.
#[test]
fn git_children_never_inherit_repository_location_env() {
    let mut cmd = std::process::Command::new("true");
    cmd.env("GIT_WORK_TREE", "/somewhere/else");
    crate::shell::apply_filtered_path(&mut cmd);
    for var in crate::shell::GIT_LOCATION_ENV {
        let removed = cmd
            .get_envs()
            .any(|(k, v)| k == std::ffi::OsStr::new(var) && v.is_none());
        assert!(removed, "{var} must be removed from every git child's env");
    }
}

// F4: thresholds -- more than 20 files, or more than 25% of tracked files.
#[test]
fn worktree_mass_deletion_trips_on_count_or_share_and_not_below() {
    let repo = seeded_repo(100);
    let item = test_item(686);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    assert_eq!(worktree_mass_deletion(&wt), None, "clean tree");

    for i in 0..20 {
        std::fs::remove_file(wt.join(format!("f{i}.txt"))).unwrap();
    }
    assert_eq!(
        worktree_mass_deletion(&wt),
        None,
        "20 of 100 is neither more than 20 files nor more than 25%"
    );
    std::fs::remove_file(wt.join("f20.txt")).unwrap();
    assert_eq!(
        worktree_mass_deletion(&wt),
        Some(MassDeletion {
            deleted: 21,
            tracked: 100
        })
    );

    let small = seeded_repo(8);
    let small_wt = create_worktree(&item, &small.path, "master", None).unwrap();
    std::fs::remove_file(small_wt.join("f0.txt")).unwrap();
    std::fs::remove_file(small_wt.join("f1.txt")).unwrap();
    assert_eq!(
        worktree_mass_deletion(&small_wt),
        None,
        "2 of 8 is exactly 25%"
    );
    std::fs::remove_file(small_wt.join("f2.txt")).unwrap();
    assert_eq!(
        worktree_mass_deletion(&small_wt),
        Some(MassDeletion {
            deleted: 3,
            tracked: 8
        })
    );
}

#[test]
fn branch_mass_deletion_sees_a_committed_wipe_but_not_renames_or_an_untouched_branch() {
    let repo = seeded_repo(30);
    let item = test_item(686);
    let wt = create_worktree(&item, &repo.path, "master", None).unwrap();
    let branch = task_branch_name(&item);
    assert_eq!(
        branch_mass_deletion(&repo.path, &branch, "master"),
        None,
        "a branch with no commits of its own has nothing to refuse"
    );

    std::fs::create_dir_all(wt.join("moved")).unwrap();
    for i in 0..30 {
        let name = format!("f{i}.txt");
        std::fs::rename(wt.join(&name), wt.join("moved").join(&name)).unwrap();
    }
    run_git_in(&wt, &["add", "-A"]).unwrap();
    run_git_in(&wt, &["commit", "-m", "move everything"]).unwrap();
    assert_eq!(
        branch_mass_deletion(&repo.path, &branch, "master"),
        None,
        "renames are not deletions"
    );

    std::fs::remove_dir_all(wt.join("moved")).unwrap();
    run_git_in(&wt, &["add", "-A"]).unwrap();
    run_git_in(&wt, &["commit", "-m", "wip(sdd-loop): task 0 checkpoint"]).unwrap();
    assert_eq!(
        branch_mass_deletion(&repo.path, &branch, "master"),
        Some(MassDeletion {
            deleted: 30,
            tracked: 30
        })
    );

    // A later commit on top does not launder the wipe.
    std::fs::write(wt.join("new.txt"), "x").unwrap();
    run_git_in(&wt, &["add", "-A"]).unwrap();
    run_git_in(&wt, &["commit", "-m", "more"]).unwrap();
    assert!(branch_mass_deletion(&repo.path, &branch, "master").is_some());
}
