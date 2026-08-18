use super::*;
use crate::shell::test_support::{Repo, init_repo_with_branch};
use tempfile::TempDir;

fn init_repo() -> Repo {
    init_repo_with_branch("master")
}

/// item #78: on Windows, `kill_tree` fires `taskkill /T /F` and trusts
/// its process-tree snapshot blindly — it never verifies a grandchild
/// is actually gone before `run_output_timeout` returns. If that
/// snapshot misses a grandchild (spawned in the brief window between
/// the snapshot and termination), it keeps running unsupervised, which
/// on a resource-constrained CI runner is exactly the kind of
/// background load that can starve whatever test nextest schedules
/// next (see item #78's investigation). Poll for `image_name`
/// processes whose command line contains `cmdline_needle` so a
/// regression here fails loudly and locally instead of surfacing as an
/// unrelated test's mystery timeout.
#[cfg(windows)]
fn assert_no_surviving_process(image_name: &str, cmdline_needle: &str) {
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        let ps_query = format!(
            "(Get-CimInstance Win32_Process -Filter \"Name='{image_name}'\" | \
             Where-Object {{ $_.CommandLine -like '*{cmdline_needle}*' }} | \
             Select-Object -ExpandProperty ProcessId) -join ','"
        );
        let out = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps_query])
            .output();
        let survivors = out
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default();
        if survivors.is_empty() {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "kill_tree left {image_name} process(es) matching \
                 '{cmdline_needle}' running after timeout (pid(s): {survivors}) \
                 — the process tree was not fully terminated"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn remove_worktree_dir_succeeds_immediately_when_unlocked() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("wt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("file.txt"), b"hello").unwrap();
    assert!(remove_worktree_dir(&dir, "test"));
    assert!(!dir.exists());
}

#[cfg(windows)]
#[test]
fn remove_worktree_dir_clears_a_genuine_acl_denial() {
    // item #267's actual failure mode: cargo's own target/*/.fingerprint/*
    // files can end up ACL-restricted, not just transiently open. Neither
    // the retry loop nor `cmd /c rmdir` clears a real ACL deny -- only
    // `icacls /grant ... /T` does.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("wt");
    std::fs::create_dir_all(&dir).unwrap();
    let locked_file = dir.join("locked.txt");
    std::fs::write(&locked_file, b"hello").unwrap();

    let user = std::env::var("USERNAME").unwrap();
    let deny_args: Vec<String> = vec![
        locked_file.to_string_lossy().to_string(),
        "/deny".to_string(),
        format!("{user}:(D)"),
    ];
    let status = std::process::Command::new("icacls")
        .args(&deny_args)
        .status()
        .unwrap();
    assert!(status.success(), "test setup: icacls /deny must succeed");

    assert!(
        remove_worktree_dir(&dir, "test"),
        "must clear the ACL denial via icacls /grant, not just retry"
    );
    assert!(!dir.exists());
}

#[test]
fn remove_worktree_dir_retries_past_a_transient_lock() {
    // On Windows, an open file handle blocks remove_dir_all with
    // Permission denied until released -- exactly item #302's failure
    // mode (rust-analyzer holding a worktree file open). On Unix this
    // doesn't block deletion at all, so the test still passes there,
    // just without exercising the retry path meaningfully.
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("wt");
    std::fs::create_dir_all(&dir).unwrap();
    let locked_file = dir.join("locked.txt");
    std::fs::write(&locked_file, b"hello").unwrap();

    let file = std::fs::File::open(&locked_file).unwrap();
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(250));
        drop(file);
    });

    assert!(
        remove_worktree_dir(&dir, "test"),
        "must succeed once the lock clears, within the retry budget"
    );
    assert!(!dir.exists());
    releaser.join().unwrap();
}

fn test_item(sequence_id: i64) -> Item {
    Item {
        id: "test-id".into(),
        project_id: "proj".into(),
        state_id: "state".into(),
        name: "test".into(),
        description: String::new(),
        priority: "none".into(),
        parent_id: None,
        assignee_agent: None,
        sequence_id,
        sort_order: 0.0,
        started_at: None,
        completed_at: None,
        archived_at: None,
        external_source: None,
        external_id: None,
        metadata: "{}".into(),
        created_at: 0,
        updated_at: 0,
        deleted_at: None,
    }
}

fn test_item_named(sequence_id: i64, name: &str) -> Item {
    Item {
        name: name.into(),
        ..test_item(sequence_id)
    }
}

#[test]
fn task_branch_name_slugs_a_normal_title() {
    let item = test_item_named(33, "Agentflare code review");
    assert_eq!(task_branch_name(&item), "task/33-agentflare-code-review");
}

#[test]
fn task_branch_name_falls_back_to_bare_form_for_an_empty_title() {
    let item = test_item_named(33, "");
    assert_eq!(task_branch_name(&item), "task/33");
}

#[test]
fn task_branch_name_falls_back_to_bare_form_for_an_all_symbol_or_unicode_title() {
    let item = test_item_named(33, "!!! -- 日本語 --- !!!");
    assert_eq!(task_branch_name(&item), "task/33");
}

#[test]
fn task_branch_name_truncates_a_very_long_title() {
    let item = test_item_named(33, &"word ".repeat(30));
    let branch = task_branch_name(&item);
    assert!(
        branch.len() <= "task/33-".len() + MAX_SLUG_LEN,
        "branch name not truncated: {branch} ({} chars)",
        branch.len()
    );
    assert!(!branch.ends_with('-'));
}

#[test]
fn ensure_worktrees_ignored_is_noop_when_already_ignored() {
    let repo = init_repo();
    let exclude_path = repo.path.join(".git").join("info").join("exclude");
    std::fs::create_dir_all(exclude_path.parent().unwrap()).unwrap();
    std::fs::write(&exclude_path, ".worktrees/\n.cargo/\n").unwrap();
    let before = std::fs::read_to_string(&exclude_path).unwrap();
    ensure_worktrees_ignored(&repo.path);
    let after = std::fs::read_to_string(&exclude_path).unwrap();
    assert_eq!(before, after);
}

#[test]
fn ensure_worktrees_ignored_adds_only_the_missing_pattern() {
    // A file that already has one pattern but not the other must gain
    // only what's missing, not duplicate the one already present.
    let repo = init_repo();
    let exclude_path = repo.path.join(".git").join("info").join("exclude");
    std::fs::create_dir_all(exclude_path.parent().unwrap()).unwrap();
    std::fs::write(&exclude_path, ".worktrees/\n").unwrap();
    ensure_worktrees_ignored(&repo.path);
    let content = std::fs::read_to_string(&exclude_path).unwrap();
    assert_eq!(content.matches(".worktrees/").count(), 1);
    assert_eq!(content.matches(".cargo/").count(), 1);
}

#[test]
fn ensure_worktrees_ignored_adds_to_local_exclude_without_committing() {
    let repo = init_repo();
    ensure_worktrees_ignored(&repo.path);
    let exclude_path = repo.path.join(".git").join("info").join("exclude");
    let content = std::fs::read_to_string(&exclude_path).unwrap();
    assert!(content.contains(".worktrees/"));
    assert!(content.contains(".cargo/"));
    // Must never touch the tracked .gitignore or create a commit.
    assert!(!repo.path.join(".gitignore").exists());
    let log = run_git_in(&repo.path, &["log", "--oneline"]).unwrap();
    assert_eq!(
        log.lines().count(),
        1,
        "no new commit should have been made"
    );
}

#[test]
fn already_isolated_for_false_in_regular_repo() {
    let repo = init_repo();
    assert!(!already_isolated_for("task/1", &repo.path));
}

#[test]
fn isolate_worktree_target_dir_writes_relative_target_dir() {
    let tmp = TempDir::new().unwrap();
    let wt = tmp.path().join(".worktrees").join("task").join("1");
    std::fs::create_dir_all(&wt).unwrap();
    isolate_worktree_target_dir(&wt);
    let config = wt.join(".cargo").join("config.toml");
    assert!(config.exists(), "expected .cargo/config.toml in worktree");
    let content = std::fs::read_to_string(&config).unwrap();
    assert!(
        content.contains("target-dir = \"target\""),
        "must set a relative, per-checkout target dir, got: {content}"
    );
    assert!(
        !content.contains("target-dir = \"/")
            && !content.contains("target-dir = \"~")
            && !content.contains("CARGO_TARGET_DIR ="),
        "must not set an absolute/shared target dir"
    );
}

#[test]
fn isolate_worktree_target_dir_does_not_clobber_existing_config() {
    let tmp = TempDir::new().unwrap();
    let wt = tmp.path().join(".worktrees").join("task").join("1");
    let cargo_dir = wt.join(".cargo");
    std::fs::create_dir_all(&cargo_dir).unwrap();
    let config = cargo_dir.join("config.toml");
    std::fs::write(
        &config,
        "[build]\ntarget-dir = \"/some/intentional/path\"\n",
    )
    .unwrap();
    isolate_worktree_target_dir(&wt);
    let content = std::fs::read_to_string(&config).unwrap();
    assert!(
        content.contains("/some/intentional/path"),
        "existing worktree-local config must be preserved"
    );
}

#[test]
fn isolate_worktree_target_dir_wires_sccache_when_available() {
    let tmp = TempDir::new().unwrap();
    let wt = tmp.path().join(".worktrees").join("task").join("1");
    std::fs::create_dir_all(&wt).unwrap();
    isolate_worktree_target_dir(&wt);
    let config = wt.join(".cargo").join("config.toml");
    let content = std::fs::read_to_string(&config).unwrap();
    if sccache_available() {
        assert!(
            content.contains("rustc-wrapper = \"sccache\""),
            "expected sccache wired up as rustc-wrapper, got: {content}"
        );
        let escaped = wt
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        let basedir_line = format!("SCCACHE_BASEDIRS = \"{escaped}\"");
        assert!(
            content.contains(&basedir_line),
            "expected SCCACHE_BASEDIRS to strip this worktree's own path, got: {content}"
        );
    } else {
        assert!(
            !content.contains("rustc-wrapper") && !content.contains("SCCACHE_BASEDIRS"),
            "must not reference sccache when it isn't on PATH, got: {content}"
        );
    }
}

#[test]
fn warn_if_ambient_target_dir_warns_when_set() {
    // Just asserts the function runs without panicking whether or not the
    // var is set; the warning is an ephemeral eprintln, not assertable here.
    unsafe {
        std::env::set_var("CARGO_TARGET_DIR", "/tmp/shared");
    }
    warn_if_ambient_target_dir();
    unsafe {
        std::env::remove_var("CARGO_TARGET_DIR");
    }
    warn_if_ambient_target_dir();
}

#[test]
fn already_isolated_for_true_inside_the_worktree_it_created() {
    let repo = init_repo();
    let item = test_item(1);
    let target = resolve_default_branch(&repo.path);
    let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();
    assert!(already_isolated_for(
        &task_branch_name(&item),
        &worktree_path
    ));
}

#[test]
fn create_worktree_creates_worktree_and_branch() {
    let repo = init_repo();
    let worktree_path = repo.path.join(".worktrees").join("task").join("1");
    let item = test_item(1);
    let target = resolve_default_branch(&repo.path);
    let result = create_worktree(&item, &repo.path, &target, None);
    assert!(result.is_ok());
    assert!(worktree_path.exists());
}

#[test]
fn cleanup_item_worktree_removes_a_clean_one() {
    let repo = init_repo();
    let item = test_item(1);
    let target = resolve_default_branch(&repo.path);
    let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();
    assert!(worktree_path.exists());

    assert!(cleanup_item_worktree(&item, &repo.path));
    assert!(!worktree_path.exists());
}

#[test]
fn cleanup_item_worktree_refuses_a_dirty_one() {
    let repo = init_repo();
    let item = test_item(1);
    let target = resolve_default_branch(&repo.path);
    let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();
    std::fs::write(worktree_path.join("uncommitted.txt"), "x").unwrap();

    assert!(!cleanup_item_worktree(&item, &repo.path));
    assert!(worktree_path.exists());
    assert!(worktree_path.join("uncommitted.txt").exists());
}

#[test]
fn cleanup_item_worktree_is_a_noop_when_none_exists() {
    let repo = init_repo();
    let item = test_item(1);
    assert!(!cleanup_item_worktree(&item, &repo.path));
}

#[test]
fn audit_orphans_flags_a_worktree_stranded_on_the_default_branch() {
    let repo = init_repo();
    let item = test_item(7);
    let target = resolve_default_branch(&repo.path);
    let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();
    assert!(worktree_path.exists());
    // Free the default branch from the canonical checkout first -- git
    // refuses to check out the same branch in two worktrees (this exact
    // refusal is the gh merge-collision from item #441).
    assert!(crate::shell::run_in_ok(
        &repo.path,
        &["switch", "-c", "canonical/other"]
    ));
    // The stranded-worktree failure mode: a task worktree that has been
    // switched onto the default branch (intact gitdir, not broken).
    crate::shell::run_in(&worktree_path, &["switch", "master"]).unwrap();

    let claimed: std::collections::HashSet<String> =
        std::collections::HashSet::from(["7".to_string()]);
    // Claimed -> not an orphan.
    assert!(audit_orphans(&repo.path, Some(&claimed)).is_empty());
    // Not claimed -> orphan, flagged as on the default branch, with an
    // intact gitdir.
    let orphans = audit_orphans(&repo.path, Some(&std::collections::HashSet::new()));
    assert_eq!(orphans.len(), 1);
    assert!(!orphans[0].has_broken_gitdir);
    assert!(orphans[0].on_default_branch);
}

#[test]
fn audit_orphans_preserves_a_dirty_worktree_stranded_on_the_default_branch() {
    let repo = init_repo();
    let item = test_item(9);
    let target = resolve_default_branch(&repo.path);
    let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();
    assert!(worktree_path.exists());
    assert!(crate::shell::run_in_ok(
        &repo.path,
        &["switch", "-c", "canonical/other"]
    ));
    crate::shell::run_in(&worktree_path, &["switch", "master"]).unwrap();
    // Uncommitted changes -- real work that must survive an audit/prune
    // sweep even though the worktree is stranded on the default branch.
    std::fs::write(worktree_path.join("dirty.txt"), "not committed").unwrap();

    let orphans = audit_orphans(&repo.path, Some(&std::collections::HashSet::new()));
    assert!(
        orphans.is_empty(),
        "dirty stranded worktree must not be listed as prunable: {} found",
        orphans.len()
    );
}

#[test]
fn audit_orphans_ignores_a_claimed_or_own_branch_worktree() {
    let repo = init_repo();
    let item = test_item(8);
    let target = resolve_default_branch(&repo.path);
    let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();
    assert!(worktree_path.exists());

    // Own task/<N> branch, intact gitdir, unclaimed-but-live -- not an
    // orphan (no broken gitdir, not on the default branch).
    let orphans = audit_orphans(&repo.path, Some(&std::collections::HashSet::new()));
    assert!(orphans.is_empty());
}

#[test]
fn commit_uncommitted_commits_a_dirty_worktree() {
    let repo = init_repo();
    let item = test_item(1);
    let target = resolve_default_branch(&repo.path);
    let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();
    std::fs::write(worktree_path.join("forgot_to_commit.txt"), "x").unwrap();

    assert!(matches!(
        commit_uncommitted(&item, &repo.path, "auto-committed"),
        CommitOutcome::Committed
    ));

    let status = run_git_in(&worktree_path, &["status", "--porcelain"]).unwrap();
    assert!(status.is_empty(), "worktree must be clean after commit");
    let branch = task_branch_name(&item);
    assert!(branch_diverged(&repo.path, &branch, &target));
}

#[test]
fn commit_uncommitted_is_a_noop_on_a_clean_worktree() {
    let repo = init_repo();
    let item = test_item(1);
    let target = resolve_default_branch(&repo.path);
    create_worktree(&item, &repo.path, &target, None).unwrap();

    assert!(matches!(
        commit_uncommitted(&item, &repo.path, "auto-committed"),
        CommitOutcome::NothingToCommit
    ));
    assert!(!branch_diverged(
        &repo.path,
        &task_branch_name(&item),
        &target
    ));
}

#[test]
fn commit_uncommitted_is_a_noop_when_no_worktree_exists() {
    let repo = init_repo();
    let item = test_item(1);
    assert!(matches!(
        commit_uncommitted(&item, &repo.path, "auto-committed"),
        CommitOutcome::NothingToCommit
    ));
}

// Windows' FILE_ATTRIBUTE_READONLY on a *directory* doesn't prevent writes
// to files inside it (unlike Unix permission bits) -- the readonly-git-dir
// simulation below relies on that Unix-specific behavior to make `git add`
// fail deterministically, so on Windows the commit silently succeeds
// instead of failing, and the test's premise doesn't hold there.
#[cfg(unix)]
#[test]
fn commit_uncommitted_reports_failed_not_nothing_to_commit_when_add_fails() {
    // Item #92: a dirty tree where `git add`/`git commit` itself fails
    // (e.g. item #88's read-only `.git` under bwrap) must be
    // distinguishable from a clean tree with nothing to do -- both used
    // to collapse to the same `false`, so a real, stranded change looked
    // identical to a genuine no-op from the caller's side.
    let repo = init_repo();
    let item = test_item(1);
    let target = resolve_default_branch(&repo.path);
    let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();
    std::fs::write(worktree_path.join("forgot_to_commit.txt"), "x").unwrap();

    // Make the index unwritable so `git add -A` fails deterministically
    // while `git status` (read-only) still succeeds.
    let index_path = worktree_path.join(".git");
    let git_dir = std::fs::read_to_string(&index_path)
        .ok()
        .and_then(|s| s.strip_prefix("gitdir: ").map(str::trim).map(String::from))
        .map(PathBuf::from)
        .unwrap_or(index_path);
    let original_perms = std::fs::metadata(&git_dir).unwrap().permissions();
    let mut perms = original_perms.clone();
    perms.set_readonly(true);
    std::fs::set_permissions(&git_dir, perms).unwrap();

    let result = commit_uncommitted(&item, &repo.path, "auto-committed");

    // Restore the original permissions (not a blanket writable toggle,
    // which on Unix would leave the dir world-writable) so the temp dir
    // can be cleaned up.
    std::fs::set_permissions(&git_dir, original_perms).unwrap();

    assert!(
        matches!(result, CommitOutcome::Failed(_)),
        "a dirty tree whose add/commit fails must report Failed, not NothingToCommit"
    );
}

#[test]
fn create_worktree_reuses_a_branch_that_exists_with_no_owning_worktree() {
    // The task/29 collision: a plain local branch survives (e.g. from a
    // prior session) with no worktree ever created for it. `-b` alone
    // would fail here ("a branch named 'task/1-test' already exists") --
    // create_worktree must detect this and check out the existing
    // branch instead of trying to recreate it.
    let repo = init_repo();
    let item = test_item(1);
    let branch = task_branch_name(&item);
    run_git_in(&repo.path, &["branch", &branch]).unwrap();
    let worktree_path = repo.path.join(".worktrees").join("task").join("1");
    let target = resolve_default_branch(&repo.path);
    let result = create_worktree(&item, &repo.path, &target, None);
    assert!(result.is_ok(), "{:?}", result.err());
    assert!(worktree_path.exists());
    let checked_out = run_git_in(&worktree_path, &["branch", "--show-current"]).unwrap();
    assert_eq!(checked_out, branch);
}

#[test]
fn create_worktree_reuses_an_existing_worktree_when_called_from_the_repo_root() {
    // The daemon always calls create_worktree from the main repo root,
    // never from inside the worktree itself, so already_isolated_for's
    // "am I currently inside that worktree?" check (compares --git-dir
    // vs --git-common-dir on repo_root) can never fire for a real
    // re-claim -- it only ever returns true for the recursive case
    // where a caller happens to already be cd'd into the worktree.
    // Without an on-disk check, the second call falls through to
    // `git worktree add`, which refuses to re-add a branch that's
    // already checked out elsewhere ("bad git state?" in item #43/#30).
    let repo = init_repo();
    let item = test_item(1);
    let target = resolve_default_branch(&repo.path);
    let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();

    let result = create_worktree(&item, &repo.path, &target, None);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(result.unwrap(), worktree_path);
    assert!(worktree_path.exists());
    let checked_out = run_git_in(&worktree_path, &["branch", "--show-current"]).unwrap();
    assert_eq!(checked_out, task_branch_name(&item));
}

#[test]
fn create_worktree_reuses_a_legacy_bare_branch_worktree_predating_the_slug_scheme() {
    // item #447's actual failure: its worktree was created (by hand, or by
    // an older agentflare build) on the bare `task/<seq>` branch, before
    // the slugged `task/<seq>-<slug>` naming scheme existed. A later
    // re-claim recomputes a slugged name from the item's title and, prior
    // to this fix, missed the existing worktree entirely -- `git worktree
    // add` then collided with the already-occupied path (item #459).
    let repo = init_repo();
    let worktree_path = repo.path.join(".worktrees").join("task").join("1");
    std::fs::create_dir_all(worktree_path.parent().unwrap()).unwrap();
    run_git_in(
        &repo.path,
        &[
            "worktree",
            "add",
            "-b",
            "task/1",
            worktree_path.to_str().unwrap(),
        ],
    )
    .unwrap();

    let item = test_item_named(1, "EPIC: a real title with words to slug");
    let target = resolve_default_branch(&repo.path);
    let result = create_worktree(&item, &repo.path, &target, None);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(result.unwrap(), worktree_path);
    let checked_out = run_git_in(&worktree_path, &["branch", "--show-current"]).unwrap();
    assert_eq!(
        checked_out, "task/1",
        "must reuse the legacy bare branch, not collide trying to create a slugged one"
    );
}

#[test]
fn create_worktree_reuses_an_existing_worktree_when_the_items_title_changed() {
    // A renamed item's freshly-recomputed slug no longer matches what's
    // already checked out -- the same collision as the legacy-branch case
    // above, but triggered by an ordinary title edit instead of an old
    // naming scheme.
    let repo = init_repo();
    let target = resolve_default_branch(&repo.path);
    let original = test_item_named(1, "original title");
    let worktree_path = create_worktree(&original, &repo.path, &target, None).unwrap();
    let original_branch = run_git_in(&worktree_path, &["branch", "--show-current"]).unwrap();
    assert_eq!(original_branch, "task/1-original-title");

    let renamed = test_item_named(1, "a completely different title");
    let result = create_worktree(&renamed, &repo.path, &target, None);

    assert!(result.is_ok(), "{:?}", result.err());
    assert_eq!(result.unwrap(), worktree_path);
    let checked_out = run_git_in(&worktree_path, &["branch", "--show-current"]).unwrap();
    assert_eq!(
        checked_out, original_branch,
        "must reuse the branch the worktree is actually on, not the freshly-slugged one"
    );
}

#[test]
fn create_worktree_soft_fails_on_bad_git() {
    let tmp = TempDir::new().unwrap();
    let bad_root = tmp.path().join("not-a-repo");
    std::fs::create_dir_all(&bad_root).unwrap();
    let item = test_item(1);
    let result = create_worktree(&item, &bad_root, "master", None);
    let err = result.expect_err("not a git repo, worktree add must fail");
    assert!(
        err.contains(&item.id),
        "error must name the item it failed for: {err}"
    );
    assert!(
        err.len() > format!("worktree: creation skipped for item {}: ", item.id).len(),
        "error must carry the underlying git failure detail, not just the prefix: {err}"
    );
}

#[test]
fn create_worktree_recovers_when_branch_is_tied_to_a_stale_registration() {
    // Item #511's actual failure: the worktree directory is removed
    // out-of-band (crash, manual cleanup, disk issue) without going through
    // `git worktree remove`, so git's `.git/worktrees/<slug>/` administrative
    // entry survives and still associates the branch with the now-missing
    // path. `worktree_already_checked_out` can't see this (its first check
    // is `worktree_path.is_dir()`, which is false here), so without a prune
    // first, the fallthrough `git worktree add <path> <branch>` refuses --
    // git still considers the branch checked out elsewhere -- and that
    // failed `add` itself regenerates a fresh broken registration, so every
    // subsequent attempt reproduces the identical failure forever (confirmed
    // live on item #331: three consecutive dispatches, three identical
    // failures, each regenerating the same broken state).
    let repo = init_repo();
    let item = test_item(1);
    let target = resolve_default_branch(&repo.path);
    let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();
    assert!(worktree_path.exists());

    // Simulate the out-of-band removal: delete the directory directly,
    // bypassing `git worktree remove`/`cleanup_item_worktree`, so git's own
    // administrative entry is left dangling.
    std::fs::remove_dir_all(&worktree_path).unwrap();
    assert!(!worktree_path.exists());

    let result = create_worktree(&item, &repo.path, &target, None);

    assert!(
        result.is_ok(),
        "must recover by pruning the stale registration, not fail again: {:?}",
        result.err()
    );
    assert_eq!(result.unwrap(), worktree_path);
    assert!(worktree_path.exists());
    let checked_out = run_git_in(&worktree_path, &["branch", "--show-current"]).unwrap();
    assert_eq!(checked_out, task_branch_name(&item));
}

#[test]
fn create_worktree_fetches_target_branch_and_includes_remote_only_commits() {
    // "remote" — plays the role of `origin`.
    let remote = init_repo();
    // "local" — a clone that will go stale the moment `remote` gets a
    // new commit; this is what `create_worktree` actually operates on.
    let local_container = TempDir::new().unwrap();
    let local_path = local_container.path().join("local");
    run_git_in(
        local_container.path(),
        &[
            "clone",
            remote.path.to_str().unwrap(),
            local_path.to_str().unwrap(),
        ],
    )
    .unwrap();
    run_git_in(&local_path, &["config", "user.email", "test@test.com"]).unwrap();
    run_git_in(&local_path, &["config", "user.name", "Test"]).unwrap();

    // Lands on the remote *after* the clone — local's own `master` and
    // `origin/master` are both stale relative to this.
    run_git_in(
        &remote.path,
        &["commit", "--allow-empty", "-m", "remote-only commit"],
    )
    .unwrap();
    let remote_head = run_git_in(&remote.path, &["rev-parse", "HEAD"]).unwrap();

    let item = test_item(1);
    let worktree_path = create_worktree(&item, &local_path, "master", None).unwrap();
    let worktree_head = run_git_in(&worktree_path, &["rev-parse", "HEAD"]).unwrap();

    assert_eq!(
        worktree_head, remote_head,
        "worktree must be based on the freshly-fetched remote commit, not the stale local ref"
    );
}

#[test]
fn push_branch_returns_none_when_no_worktree_exists() {
    let repo = init_repo();
    let item = test_item(1);
    assert!(push_branch(&item, &repo.path, "master", None).is_none());
}

#[test]
fn push_branch_returns_none_when_branch_has_no_new_commits() {
    let repo = init_repo();
    let item = test_item(1);
    let target = resolve_default_branch(&repo.path);
    create_worktree(&item, &repo.path, &target, None).unwrap();
    // No commits were made in the worktree — nothing to push, so this
    // must return early without attempting a real `git push` (which
    // would fail anyway: no remote configured here).
    assert!(push_branch(&item, &repo.path, &target, None).is_none());
}

#[test]
fn push_branch_returns_none_when_branch_content_already_merged() {
    let repo = init_repo();
    let item = test_item(1);
    let target = resolve_default_branch(&repo.path);
    let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();
    let test_file = worktree_path.join("test.txt");
    std::fs::write(&test_file, b"hello").unwrap();
    run_git_in(&worktree_path, &["add", "test.txt"]).unwrap();
    run_git_in(&worktree_path, &["commit", "-m", "worktree change"]).unwrap();
    // The item's branch now has a commit master doesn't. Squash-merge:
    // cherry-pick the *diff* onto master so content matches but
    // ancestry doesn't.
    run_git_in(&repo.path, &["cherry-pick", "-n", &task_branch_name(&item)]).unwrap();
    run_git_in(&repo.path, &["commit", "-m", "squash-merge"]).unwrap();
    // Content-diff guard should catch this even though commit-count
    // guard passes.
    assert!(push_branch(&item, &repo.path, &target, None).is_none());
}

#[test]
fn run_output_timeout_kills_the_child_not_just_abandons_it() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("marker");
    // A command that, left alone, outlives our 1s timeout and then
    // writes `marker`. If the timeout only abandoned the child (the bug
    // this replaced) rather than killing it, the marker would still
    // show up once the sleep finishes on its own.
    #[cfg(unix)]
    let (program, owned_args): (&str, Vec<String>) = (
        "sh",
        vec![
            "-c".into(),
            format!("sleep 3 && touch {}", marker.display()),
        ],
    );
    #[cfg(windows)]
    let (program, owned_args): (&str, Vec<String>) = (
        "cmd",
        vec![
            "/C".into(),
            format!(
                "ping 127.0.0.1 -n 4 >NUL & echo done > {}",
                marker.display()
            ),
        ],
    );
    let args: Vec<&str> = owned_args.iter().map(String::as_str).collect();

    let result = run_output_timeout(program, &args, tmp.path(), 1);
    assert!(
        matches!(&result, Err(e) if e.contains("timed out")),
        "{result:?}"
    );

    // Checked immediately, before the natural-duration sleep below can
    // let a leaked grandchild finish on its own and mask the leak (see
    // `assert_no_surviving_process`'s doc comment, item #78).
    #[cfg(windows)]
    assert_no_surviving_process("ping.exe", "-n 4");

    // Give the command's natural (un-killed) duration time to elapse
    // before checking the marker never showed up.
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        !marker.exists(),
        "child kept running after the timeout — it was abandoned, not killed"
    );
}

#[test]
fn fetch_with_retry_recovers_from_a_single_transient_failure() {
    // Item #495/#483 incident: a one-off failed `git fetch` silently seeded
    // a new branch off a stale local ref, because there was no retry. This
    // pins the fix — a command that fails once and succeeds the second time
    // must come back `Ok` with a successful exit status, not the first
    // attempt's failure.
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("attempted");
    #[cfg(unix)]
    let (program, owned_args): (&str, Vec<String>) = (
        "sh",
        vec![
            "-c".into(),
            format!(
                "if [ -f {0} ]; then exit 0; else touch {0}; exit 1; fi",
                marker.display()
            ),
        ],
    );
    #[cfg(windows)]
    let (program, owned_args): (&str, Vec<String>) = (
        "cmd",
        vec![
            "/C".into(),
            format!(
                "if exist {0} (exit /b 0) else (echo x > {0} & exit /b 1)",
                marker.display()
            ),
        ],
    );
    let args: Vec<&str> = owned_args.iter().map(String::as_str).collect();

    let result = fetch_with_retry(program, &args, tmp.path(), 5);
    let out = result.expect("second attempt must succeed and be returned");
    assert!(
        out.status.success(),
        "retry must return the SECOND attempt's success, not the first attempt's failure"
    );
}

#[test]
fn fetch_with_retry_returns_the_final_failure_when_every_attempt_fails() {
    let tmp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    let (program, owned_args): (&str, Vec<String>) = ("sh", vec!["-c".into(), "exit 1".into()]);
    #[cfg(windows)]
    let (program, owned_args): (&str, Vec<String>) = ("cmd", vec!["/C".into(), "exit /b 1".into()]);
    let args: Vec<&str> = owned_args.iter().map(String::as_str).collect();

    let result = fetch_with_retry(program, &args, tmp.path(), 5);
    let out = result.expect("a completed process is Ok even on non-zero exit");
    assert!(
        !out.status.success(),
        "a persistently-failing fetch must still surface as a failure after the retry"
    );
}
