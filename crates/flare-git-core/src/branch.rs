//! Branch resolution and the protected-branch predicate — the latter is
//! shared by both agentflare's own PreToolUse branch guard and the new
//! git-shim's command classifier, so "is this branch protected" has exactly
//! one definition.

use crate::shell::{run_in_opt, run_in_ok};
use std::path::{Path, PathBuf};

/// Current branch name (`HEAD` in detached-HEAD state). `None` outside a git
/// repo or if git isn't on `PATH`.
#[must_use]
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
#[must_use]
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
    run_in_opt(repo_root, &["symbolic-ref", "--short", "HEAD"]).unwrap_or_else(|| "master".to_string())
}

/// `git rev-parse --show-toplevel` from `start` — handles worktrees/submodules
/// correctly, works regardless of subdirectory. `None` outside a git repo.
#[must_use]
pub fn repo_toplevel(start: &Path) -> Option<PathBuf> {
    run_in_opt(start, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// `true` if `pattern` matches `branch` -- exact match, or a prefix match
/// when `pattern` ends in `*` (e.g. `"release/*"` matches `"release/1.0"`).
/// No other glob syntax; this is meant to cover "a branch and its
/// children", not a general pattern language.
fn matches_pattern(branch: &str, pattern: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => branch.starts_with(prefix),
        None => branch == pattern,
    }
}

/// `true` if `branch` is protected: the repo's resolved default branch (or
/// a bare main/master guess when resolution failed), OR matches any
/// pattern in `extra` (see `matches_pattern`). Pure -- `extra` is passed in
/// rather than read from an env var here, so this is unit-testable without
/// env-var mutation races between parallel test threads.
#[must_use]
pub fn is_protected_branch_among(branch: &str, default: Option<&str>, extra: &[String]) -> bool {
    let is_default = match default {
        Some(default) => branch == default,
        None => branch == "main" || branch == "master",
    };
    is_default || extra.iter().any(|p| matches_pattern(branch, p))
}

/// `AGENTFLARE_GIT_PROTECTED_BRANCHES`, comma-separated, parsed into a
/// pattern list -- e.g. `"main,release/*,staging"`. Empty/unset -> no
/// extra patterns.
#[must_use]
pub fn extra_protected_branches_from_env() -> Vec<String> {
    std::env::var("AGENTFLARE_GIT_PROTECTED_BRANCHES")
        .ok()
        .map(|v| {
            v.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// `true` if `branch` is protected: the repo's resolved default branch when
/// known, otherwise a bare guess against the two conventional names
/// ("main"/"master") when resolution failed entirely (no git, no remote, no
/// main/master branch found) -- plus anything matching
/// `AGENTFLARE_GIT_PROTECTED_BRANCHES`. See `is_protected_branch_among` for
/// the pure, testable core this wraps.
#[must_use]
pub fn is_protected_branch(branch: &str, default: Option<&str>) -> bool {
    is_protected_branch_among(branch, default, &extra_protected_branches_from_env())
}

/// `true` if `repo_root` is a linked worktree rather than the canonical/
/// main checkout -- `git rev-parse --git-dir` differs from
/// `--git-common-dir` only inside a linked worktree (or, rarely, a
/// submodule; this doesn't disambiguate the two, matching
/// `worktree::already_isolated_for`'s existing simpler check). `false` for
/// anything unresolvable (not a repo at all) -- "protect the canonical
/// checkout" should never fire on "couldn't tell", since that would be a
/// much broader and more surprising deny surface than intended.
#[must_use]
pub fn is_linked_worktree(repo_root: &Path) -> bool {
    match (
        run_in_opt(repo_root, &["rev-parse", "--git-dir"]),
        run_in_opt(repo_root, &["rev-parse", "--git-common-dir"]),
    ) {
        (Some(git_dir), Some(common_dir)) => git_dir != common_dir,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::test_support::init_repo_with_branch;

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
    fn is_protected_branch_prefers_default_over_hardcoded_names() {
        // A repo whose default branch is deliberately named neither
        // main nor master must still be caught via the resolved default.
        assert!(is_protected_branch("develop", Some("develop")));
        assert!(!is_protected_branch("feature/y", Some("develop")));
    }

    #[test]
    fn is_protected_branch_falls_back_to_main_or_master_when_default_unresolved() {
        assert!(is_protected_branch("master", None));
        assert!(is_protected_branch("main", None));
        assert!(!is_protected_branch("feature/x", None));
    }

    #[test]
    fn is_protected_branch_among_matches_extra_exact_and_glob_patterns() {
        let extra = vec!["staging".to_string(), "release/*".to_string()];
        assert!(is_protected_branch_among("staging", Some("main"), &extra));
        assert!(is_protected_branch_among("release/1.0", Some("main"), &extra));
        assert!(!is_protected_branch_among("release", Some("main"), &extra));
        assert!(!is_protected_branch_among("feature/x", Some("main"), &extra));
    }

    #[test]
    fn is_protected_branch_among_with_no_extra_matches_only_default() {
        assert!(is_protected_branch_among("main", Some("main"), &[]));
        assert!(!is_protected_branch_among("staging", Some("main"), &[]));
    }

    #[test]
    fn is_linked_worktree_false_in_a_regular_repo() {
        let repo = init_repo_with_branch("master");
        assert!(!is_linked_worktree(&repo.path));
    }

    #[test]
    fn is_linked_worktree_false_outside_any_repo() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(!is_linked_worktree(dir.path()));
    }

    #[test]
    fn is_linked_worktree_true_inside_an_actual_linked_worktree() {
        let repo = init_repo_with_branch("master");
        // A fresh TempDir of its own -- `repo.path.parent()` is the SHARED
        // system temp root, which every parallel test also creates
        // directories under, and can collide with a leftover path.
        let wt_parent = tempfile::TempDir::new().unwrap();
        let wt_path = wt_parent.path().join("wt-check");
        crate::shell::run_in(
            &repo.path,
            &["worktree", "add", wt_path.to_str().unwrap(), "-b", "wt-branch"],
        )
        .unwrap();
        assert!(is_linked_worktree(&wt_path));
    }
}
