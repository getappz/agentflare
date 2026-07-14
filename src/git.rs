//! Shared git-shelling primitives. Every module that needs to run `git`
//! against a repo goes through here instead of hand-rolling its own
//! `Command::new("git")` wrapper — this consolidates what had grown into
//! 5+ near-duplicate copies of the same "spawn git, trim stdout" logic
//! across mcp_server.rs, worktree.rs, hook_redirect.rs, gateway_integrations.rs,
//! and cli/review.rs.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Runs `git` in `repo_root`; `Ok(stdout)` trimmed on success, `Err(stderr)`
/// trimmed on a non-zero exit, or a process-spawn error message (git
/// missing, etc) if it couldn't even run.
pub fn run_in(repo_root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("git not available: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `run_in`, discarding the error and treating empty stdout as `None` — the
/// "best-effort, don't care why it failed" shape most callers actually want.
pub fn run_in_opt(repo_root: &Path, args: &[&str]) -> Option<String> {
    run_in(repo_root, args).ok().filter(|s| !s.is_empty())
}

/// `true` if `git <args>` exits 0 in `repo_root`; stdout/stderr don't matter.
pub fn run_in_ok(repo_root: &Path, args: &[&str]) -> bool {
    run_in(repo_root, args).is_ok()
}

/// Current branch name (`HEAD` in detached-HEAD state). `None` outside a git
/// repo or if git isn't on `PATH`.
pub fn current_branch(repo_root: &Path) -> Option<String> {
    run_in_opt(repo_root, &["rev-parse", "--abbrev-ref", "HEAD"])
}

/// Best-effort resolution of "the" default branch: prefer the remote's own
/// record of it (`origin/HEAD`'s symbolic ref, which survives a repo default
/// named anything other than main/master), then whichever of main/master
/// actually exists as a local branch, then whatever is actually checked out
/// here — so a repo naming its default branch e.g. `trunk`/`develop` still
/// resolves instead of falling through to a hardcoded guess that may not
/// even exist.
pub fn resolve_default_branch(repo_root: &Path) -> String {
    if let Some(origin_head) = run_in_opt(
        repo_root,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    ) && let Some(stripped) = origin_head.strip_prefix("origin/")
    {
        return stripped.to_string();
    }
    if run_in_ok(repo_root, &["rev-parse", "--verify", "main"]) {
        return "main".to_string();
    }
    if run_in_ok(repo_root, &["rev-parse", "--verify", "master"]) {
        return "master".to_string();
    }
    run_in_opt(repo_root, &["symbolic-ref", "--short", "HEAD"])
        .unwrap_or_else(|| "master".to_string())
}

/// `git rev-parse --show-toplevel` from `start` — handles worktrees/submodules
/// correctly, works regardless of subdirectory. `None` outside a git repo.
pub fn repo_toplevel(start: &Path) -> Option<PathBuf> {
    run_in_opt(start, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// Unified diff for `base...head` (three-dot: changes on `head` since it
/// diverged from `base`). Stdout is returned RAW, not trimmed — diff output
/// is multi-line and whitespace-significant, unlike the single-value queries
/// the rest of this module's helpers return.
pub fn diff(repo_root: &Path, base: &str, head: &str) -> Result<String, String> {
    let range = format!("{base}...{head}");
    let out = Command::new("git")
        .args(["diff", "--unified=3", &range])
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("git diff failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git diff {range}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    struct Repo {
        _dir: TempDir,
        path: PathBuf,
    }

    fn init_repo_with_branch(branch: &str) -> Repo {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        run_in(&path, &["init", "-b", branch]).unwrap();
        run_in(&path, &["config", "user.email", "test@test.com"]).unwrap();
        run_in(&path, &["config", "user.name", "Test"]).unwrap();
        run_in(&path, &["commit", "--allow-empty", "-m", "initial"]).unwrap();
        Repo { _dir: dir, path }
    }

    #[test]
    fn run_in_opt_is_none_outside_a_repo() {
        let dir = TempDir::new().unwrap();
        assert!(run_in_opt(dir.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).is_none());
    }

    #[test]
    fn run_in_ok_reflects_exit_status() {
        let repo = init_repo_with_branch("master");
        assert!(run_in_ok(&repo.path, &["rev-parse", "--verify", "master"]));
        assert!(!run_in_ok(
            &repo.path,
            &["rev-parse", "--verify", "no-such-branch"]
        ));
    }

    #[test]
    fn current_branch_reports_the_checked_out_branch() {
        let repo = init_repo_with_branch("feature/x");
        assert_eq!(current_branch(&repo.path).as_deref(), Some("feature/x"));
    }

    #[test]
    fn resolve_default_branch_resolves_from_origin_head() {
        let repo = init_repo_with_branch("master");
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
    fn repo_toplevel_finds_root_from_a_subdirectory() {
        let repo = init_repo_with_branch("master");
        let sub = repo.path.join("sub");
        std::fs::create_dir(&sub).unwrap();
        assert_eq!(
            repo_toplevel(&sub).map(|p| p.canonicalize().unwrap()),
            repo.path.canonicalize().ok(),
        );
    }

    #[test]
    fn diff_returns_untrimmed_output_across_a_change() {
        let repo = init_repo_with_branch("master");
        std::fs::write(repo.path.join("f.txt"), "hello\n").unwrap();
        run_in(&repo.path, &["add", "f.txt"]).unwrap();
        run_in(&repo.path, &["commit", "-m", "add f.txt"]).unwrap();
        let out = diff(&repo.path, "HEAD~1", "HEAD").unwrap();
        assert!(out.contains("+hello"), "{out}");
    }

    #[test]
    fn diff_reports_git_stderr_on_an_invalid_range() {
        let repo = init_repo_with_branch("master");
        let err = diff(&repo.path, "no-such-branch", "HEAD").unwrap_err();
        assert!(err.contains("no-such-branch"), "{err}");
    }
}
