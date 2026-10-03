use super::run_git_opt_timeout;
use crate::shell::test_support::{Repo, init_repo_with_branch};
use std::time::Duration;

fn init_repo() -> Repo {
    init_repo_with_branch("master")
}

#[test]
fn run_git_opt_timeout_kills_a_hung_git_within_budget() {
    let repo = init_repo();
    #[cfg(unix)]
    let hang_alias = "!sleep 30";
    #[cfg(windows)]
    let hang_alias = "!ping -n 30 127.0.0.1";
    assert!(crate::shell::run_in_ok(
        &repo.path,
        &["config", "alias.hangtest", hang_alias],
    ));
    let start = std::time::Instant::now();
    // Alias subprocess must be killed on timeout, not abandoned.
    let out = run_git_opt_timeout(&repo.path, &["hangtest"], 1, &[]);
    assert!(out.is_none());
    assert!(
        start.elapsed() < Duration::from_secs(3),
        "hung git outlived the hook budget: {:?}",
        start.elapsed()
    );
}

#[test]
fn run_git_opt_timeout_passes_extra_env() {
    let repo = init_repo();
    let git_dir = repo.path.join(".git");
    let real_index = if git_dir.is_dir() {
        git_dir.join("index")
    } else {
        std::fs::read_to_string(&git_dir)
            .map(|p| std::path::PathBuf::from(p.trim()))
            .unwrap()
            .join("index")
    };
    let tmp = tempfile::tempdir().unwrap();
    let tmp_index = tmp.path().join("index");
    std::fs::copy(&real_index, &tmp_index).unwrap();
    let head = run_git_opt_timeout(
        &repo.path,
        &["rev-parse", "HEAD"],
        5,
        &[("GIT_INDEX_FILE", tmp_index.as_os_str())],
    );
    assert!(head.is_some(), "git with GIT_INDEX_FILE should succeed");
}
