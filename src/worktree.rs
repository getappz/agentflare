use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

fn run_git_in(repo_root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("git not available: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(stderr);
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(stdout)
}

fn run_git_in_ok(repo_root: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .ok()
        .is_some_and(|o| o.status.success())
}

pub fn resolve_target_branch(
    conn: &rusqlite::Connection,
    item: &agentflare_backend::item::Item,
    repo_root: &Path,
) -> String {
    if let Some(ref parent_id) = item.parent_id
        && let Ok(parent) = agentflare_backend::item::get(conn, parent_id)
        && let Ok(meta) = serde_json::from_str::<serde_json::Value>(&parent.metadata)
        && let Some(branch) = meta.get("branch").and_then(|v| v.as_str())
    {
        return branch.to_string();
    }
    resolve_default_branch(repo_root)
}

fn resolve_default_branch(repo_root: &Path) -> String {
    if let Ok(out) = run_git_in(
        repo_root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) && let Some(stripped) = out.strip_prefix("origin/")
    {
        return stripped.to_string();
    }
    if run_git_in_ok(repo_root, &["rev-parse", "--verify", "main"]) {
        return "main".to_string();
    }
    if run_git_in_ok(repo_root, &["rev-parse", "--verify", "master"]) {
        return "master".to_string();
    }
    // Last resort: whatever branch is actually checked out here, so repos
    // using trunk/develop/anything else still get a real branch instead of
    // a hardcoded guess that may not exist.
    run_git_in(repo_root, &["symbolic-ref", "--short", "HEAD"])
        .unwrap_or_else(|_| "master".to_string())
}

pub fn already_isolated_for(branch: &str, repo_root: &Path) -> bool {
    let git_dir = match run_git_in(repo_root, &["rev-parse", "--git-dir"]) {
        Ok(d) => d,
        Err(_) => return false,
    };
    let common_dir = match run_git_in(repo_root, &["rev-parse", "--git-common-dir"]) {
        Ok(d) => d,
        Err(_) => return false,
    };
    if git_dir == common_dir {
        return false;
    }
    // Exits 0 with EMPTY stdout in a plain linked worktree (not a git
    // submodule) — only a non-empty path means we're actually inside a
    // submodule's own superproject relationship, which is the case this
    // guard exists to rule out.
    if let Ok(out) = run_git_in(
        repo_root,
        &["rev-parse", "--show-superproject-working-tree"],
    ) && !out.is_empty()
    {
        return false;
    }
    match run_git_in(repo_root, &["branch", "--show-current"]) {
        Ok(b) => b == branch,
        Err(_) => false,
    }
}

/// Adds `.worktrees/` to this repo's LOCAL, untracked ignore rules
/// (`.git/info/exclude`) rather than the tracked `.gitignore` — a claim
/// should never create a commit in the caller's repository (would sweep up
/// any unrelated staged files, and any pre-existing uncommitted `.gitignore`
/// edits, into a commit the agent didn't ask for).
pub fn ensure_worktrees_ignored(repo_root: &Path) {
    let Ok(common_dir) = run_git_in(repo_root, &["rev-parse", "--git-common-dir"]) else {
        return;
    };
    let exclude_path = repo_root.join(common_dir).join("info").join("exclude");
    if let Ok(existing) = std::fs::read_to_string(&exclude_path)
        && existing
            .lines()
            .any(|l| l.trim() == ".worktrees/" || l.trim() == ".worktrees")
    {
        return;
    }
    let mut content = String::new();
    if let Ok(existing) = std::fs::read_to_string(&exclude_path) {
        content = existing;
    }
    if !content.ends_with('\n') && !content.is_empty() {
        content.push('\n');
    }
    content.push_str(".worktrees/\n");
    if let Some(parent) = exclude_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&exclude_path, content).is_err() {
        eprintln!("worktree: failed to write .git/info/exclude");
    }
}

/// Creates an isolated git worktree for `item` against `target_branch`.
///
/// Deliberately takes an already-resolved `target_branch` instead of a
/// database connection: callers should resolve the branch (`resolve_target_branch`,
/// above) while still holding whatever lock guards the database, then call
/// this *after* releasing it. `git worktree add` is a blocking
/// filesystem+subprocess operation with no business running while a shared
/// DB lock is held.
pub fn create_worktree(
    item: &agentflare_backend::item::Item,
    repo_root: &Path,
    target_branch: &str,
) -> Option<PathBuf> {
    let branch = format!("task/{}", item.sequence_id);
    let worktree_path = repo_root
        .join(".worktrees")
        .join("task")
        .join(item.sequence_id.to_string());
    if already_isolated_for(&branch, repo_root) {
        return Some(worktree_path);
    }
    ensure_worktrees_ignored(repo_root);
    if let Some(parent) = worktree_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match run_git_in(
        repo_root,
        &[
            "worktree",
            "add",
            &worktree_path.to_string_lossy(),
            "-b",
            &branch,
            target_branch,
        ],
    ) {
        Ok(_) => Some(worktree_path),
        Err(e) => {
            eprintln!("worktree: creation skipped for item {}: {}", item.id, e);
            None
        }
    }
}

/// Pushes `item`'s isolated worktree branch and opens a PR against
/// `target_branch` — the `done`-side counterpart to `create_worktree`.
/// Deliberately never merges: unreviewed code should never land on the
/// target branch automatically, so the worktree/branch are left in place
/// for the PR to actually get reviewed and merged. Soft-fails (eprintln, no
/// error surfaced, returns `None`) on any failure — nothing here, including
/// `gh` being unavailable, should block `done` since the item's completion
/// is already committed to the DB by the time this runs.
pub fn push_and_open_pr(
    item: &agentflare_backend::item::Item,
    repo_root: &Path,
    target_branch: &str,
) -> Option<String> {
    let branch = format!("task/{}", item.sequence_id);
    let worktree_path = repo_root
        .join(".worktrees")
        .join("task")
        .join(item.sequence_id.to_string());
    if !worktree_path.exists() {
        return None; // nothing was ever claimed into a worktree for this item
    }
    // Nothing to push (and nothing worth a PR) if the branch never
    // diverged from its target — e.g. `done` called with no commits made.
    match run_git_in(
        repo_root,
        &["rev-list", "--count", &format!("{target_branch}..{branch}")],
    ) {
        Ok(count) if count != "0" => {}
        _ => return None,
    }
    if let Err(e) = run_git_in(repo_root, &["push", "-u", "origin", &branch]) {
        eprintln!("worktree: push skipped for item {}: {e}", item.id);
        return None;
    }
    let body = format!("Auto-opened on `item done` for {}.", item.id);
    match Command::new("gh")
        .args([
            "pr",
            "create",
            "--base",
            target_branch,
            "--head",
            &branch,
            "--title",
            &item.name,
            "--body",
            &body,
        ])
        .current_dir(repo_root)
        .output()
    {
        Ok(out) if out.status.success() => {
            let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if url.is_empty() { None } else { Some(url) }
        }
        Ok(out) => {
            eprintln!(
                "worktree: gh pr create failed for item {}: {}",
                item.id,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            None
        }
        Err(e) => {
            eprintln!(
                "worktree: gh unavailable, skipping PR for item {}: {e}",
                item.id
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct Repo {
        _dir: TempDir,
        path: PathBuf,
    }

    fn init_repo() -> Repo {
        init_repo_with_branch("master")
    }

    fn init_repo_with_branch(branch: &str) -> Repo {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        run_git_in(&path, &["init", "-b", branch]).unwrap();
        run_git_in(&path, &["config", "user.email", "test@test.com"]).unwrap();
        run_git_in(&path, &["config", "user.name", "Test"]).unwrap();
        run_git_in(&path, &["commit", "--allow-empty", "-m", "initial"]).unwrap();
        Repo { _dir: dir, path }
    }

    fn test_item(sequence_id: i64) -> agentflare_backend::item::Item {
        agentflare_backend::item::Item {
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

    #[test]
    fn resolve_default_branch_resolves_from_origin_head() {
        let repo = init_repo();
        assert_eq!(resolve_default_branch(&repo.path), "master");
    }

    #[test]
    fn resolve_default_branch_falls_back_to_actual_head_for_nonstandard_names() {
        // No origin, no "main", no "master" — must not guess "master" when
        // the repo's real default branch is named something else entirely.
        let repo = init_repo_with_branch("trunk");
        assert_eq!(resolve_default_branch(&repo.path), "trunk");
    }

    #[test]
    fn ensure_worktrees_ignored_is_noop_when_already_ignored() {
        let repo = init_repo();
        let exclude_path = repo.path.join(".git").join("info").join("exclude");
        std::fs::create_dir_all(exclude_path.parent().unwrap()).unwrap();
        std::fs::write(&exclude_path, ".worktrees/\n").unwrap();
        let before = std::fs::read_to_string(&exclude_path).unwrap();
        ensure_worktrees_ignored(&repo.path);
        let after = std::fs::read_to_string(&exclude_path).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn ensure_worktrees_ignored_adds_to_local_exclude_without_committing() {
        let repo = init_repo();
        ensure_worktrees_ignored(&repo.path);
        let exclude_path = repo.path.join(".git").join("info").join("exclude");
        let content = std::fs::read_to_string(&exclude_path).unwrap();
        assert!(content.contains(".worktrees/"));
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
    fn already_isolated_for_true_inside_the_worktree_it_created() {
        let repo = init_repo();
        let item = test_item(1);
        let target = resolve_default_branch(&repo.path);
        let worktree_path = create_worktree(&item, &repo.path, &target).unwrap();
        assert!(already_isolated_for("task/1", &worktree_path));
    }

    #[test]
    fn create_worktree_creates_worktree_and_branch() {
        let repo = init_repo();
        let worktree_path = repo.path.join(".worktrees").join("task").join("1");
        let item = test_item(1);
        let target = resolve_default_branch(&repo.path);
        let result = create_worktree(&item, &repo.path, &target);
        assert!(result.is_some());
        assert!(worktree_path.exists());
    }

    #[test]
    fn create_worktree_soft_fails_on_bad_git() {
        let tmp = TempDir::new().unwrap();
        let bad_root = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&bad_root).unwrap();
        let item = test_item(1);
        let result = create_worktree(&item, &bad_root, "master");
        assert!(result.is_none());
    }

    #[test]
    fn push_and_open_pr_returns_none_when_no_worktree_exists() {
        let repo = init_repo();
        let item = test_item(1);
        assert!(push_and_open_pr(&item, &repo.path, "master").is_none());
    }

    #[test]
    fn push_and_open_pr_returns_none_when_branch_has_no_new_commits() {
        let repo = init_repo();
        let item = test_item(1);
        let target = resolve_default_branch(&repo.path);
        create_worktree(&item, &repo.path, &target).unwrap();
        // No commits were made in the worktree — nothing to push, so this
        // must return early without attempting a real `git push`/`gh pr
        // create` (which would fail anyway: no remote configured here).
        assert!(push_and_open_pr(&item, &repo.path, &target).is_none());
    }
}
