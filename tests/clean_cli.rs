//! End-to-end tests for `agentflare clean` against the built binary. HOME
//! points at a temp dir so no real backend.db, vault or GitHub token is read,
//! and the fixture repo has no `origin`, so nothing touches the network.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn has_branch(dir: &Path, name: &str) -> bool {
    Command::new("git")
        .current_dir(dir)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{name}"),
        ])
        .output()
        .unwrap()
        .status
        .success()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::TempDir::new().unwrap();
    // Not canonicalized: on Windows that yields a `\\?\` verbatim path, which
    // `git worktree add` rejects. The binary canonicalizes what it compares.
    let root = tmp.path().to_path_buf();
    let (home, repo) = (root.join("home"), root.join("repo"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "master"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "T"]);
    git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
    Fixture {
        _tmp: tmp,
        home,
        repo,
    }
}

fn run(f: &Fixture, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agentflare"))
        .current_dir(&f.repo)
        .args(args)
        .env("HOME", &f.home)
        .env("USERPROFILE", &f.home)
        .env("AGENTFLARE_HOME_OVERRIDE", &f.home)
        .env("AGENTFLARE_NO_INTERACTIVE", "1")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .output()
        .unwrap()
}

fn clean(f: &Fixture, args: &[&str]) -> Output {
    let mut all = vec!["clean"];
    all.extend_from_slice(args);
    run(f, &all)
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn without_yes_nothing_is_deleted_and_the_plan_is_printed() {
    let f = fixture();
    git(&f.repo, &["branch", "done"]);
    let out = clean(&f, &[]);
    let text = stdout(&out);
    assert!(out.status.success(), "{text}\n{}", stderr(&out));
    assert!(text.contains("done"), "{text}");
    assert!(text.contains("-y"), "hint to pass -y: {text}");
    assert!(has_branch(&f.repo, "done"));
}

#[test]
fn yes_deletes_and_writes_a_restore_log() {
    let f = fixture();
    git(&f.repo, &["branch", "done"]);
    let out = clean(&f, &["-y"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!has_branch(&f.repo, "done"));
    let logs: Vec<_> = std::fs::read_dir(f.home.join(".agentflare/clean"))
        .unwrap()
        .flatten()
        .collect();
    assert_eq!(logs.len(), 1);
    let log = std::fs::read_to_string(logs[0].path()).unwrap();
    let entries: serde_json::Value = serde_json::from_str(&log).unwrap();
    assert_eq!(entries[0]["name"], "done");
    assert_eq!(entries[0]["sha"].as_str().unwrap().len(), 40);
}

#[test]
fn dry_run_json_lists_items_with_stable_ids_and_deletes_nothing() {
    let f = fixture();
    git(&f.repo, &["branch", "done"]);
    let out = clean(&f, &["--dry-run", "--json", "-y"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", stdout(&out)));
    assert_eq!(v["items"][0]["id"], "branch:done");
    assert!(v["skipped"].is_array());
    assert!(has_branch(&f.repo, "done"), "--dry-run wins over -y");
}

#[test]
fn json_apply_reports_outcomes() {
    let f = fixture();
    git(&f.repo, &["branch", "done"]);
    let out = clean(&f, &["--json", "-y"]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", stdout(&out)));
    assert_eq!(v["outcomes"][0]["id"], "branch:done");
    assert_eq!(v["outcomes"][0]["ok"], true);
    assert!(!has_branch(&f.repo, "done"));
}

#[test]
fn artifacts_are_opt_in_and_tracked_dirs_survive() {
    let f = fixture();
    std::fs::write(f.repo.join("Cargo.toml"), "[package]\nname='x'\n").unwrap();
    std::fs::write(f.repo.join("package.json"), "{}").unwrap();
    std::fs::write(f.repo.join(".gitignore"), "target/\n").unwrap();
    std::fs::create_dir_all(f.repo.join("target/debug")).unwrap();
    std::fs::write(f.repo.join("target/debug/a.o"), b"obj").unwrap();
    std::fs::create_dir_all(f.repo.join("dist")).unwrap();
    std::fs::write(f.repo.join("dist/app.js"), b"shipped").unwrap();
    git(&f.repo, &["add", "."]);
    git(&f.repo, &["commit", "-m", "files"]);

    assert!(clean(&f, &["-y"]).status.success());
    assert!(
        f.repo.join("target").exists(),
        "no --artifacts => untouched"
    );

    let out = clean(&f, &["--artifacts", "-y"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(!f.repo.join("target").exists());
    assert!(
        f.repo.join("dist/app.js").exists(),
        "a tracked dist/ is never deleted"
    );
    assert!(
        !f.repo.join(".trash").exists(),
        "the parked copy is purged and the trash dir removed"
    );
}

#[test]
fn artifacts_run_does_not_also_delete_branches() {
    let f = fixture();
    git(&f.repo, &["branch", "done"]);
    assert!(clean(&f, &["--artifacts", "-y"]).status.success());
    assert!(has_branch(&f.repo, "done"));
}

#[test]
fn only_filter_limits_what_yes_applies() {
    let f = fixture();
    git(&f.repo, &["branch", "task/1-a"]);
    git(&f.repo, &["branch", "keep-me"]);
    assert!(clean(&f, &["-y", "--only", "task/*"]).status.success());
    assert!(!has_branch(&f.repo, "task/1-a"));
    assert!(has_branch(&f.repo, "keep-me"));
}

// Review Focus 5
#[test]
fn empty_plan_exits_zero_with_a_plain_message() {
    let f = fixture();
    let out = clean(&f, &["-y"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("Nothing to clean"),
        "{}",
        stdout(&out)
    );
}

#[test]
fn refuses_the_home_directory_as_scan_root() {
    let f = fixture();
    let out = clean(&f, &["--artifacts", f.home.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("home directory"), "{}", stderr(&out));
}

#[test]
fn unknown_artifact_kind_is_rejected() {
    let f = fixture();
    let out = clean(&f, &["--artifacts=cobol"]);
    assert!(!out.status.success());
    assert!(stderr(&out).contains("cobol"), "{}", stderr(&out));
}

#[test]
fn cleanup_alias_works() {
    let f = fixture();
    git(&f.repo, &["branch", "done"]);
    let out = run(&f, &["cleanup", "--dry-run"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("done"));
}

// Review finding 1: from inside a linked worktree, the worktree you are
// standing in and the main checkout's branch are both left alone.
#[test]
fn run_from_a_linked_worktree_spares_it_and_the_main_checkouts_branch() {
    let f = fixture();
    let wt = f.repo.join(".worktrees/task/9");
    git(
        &f.repo,
        &["worktree", "add", "-b", "task/9-x", wt.to_str().unwrap()],
    );
    git(&f.repo, &["branch", "done"]);
    git(&f.repo, &["switch", "-c", "feature-x"]);

    let out = Command::new(env!("CARGO_BIN_EXE_agentflare"))
        .current_dir(&wt)
        .args(["clean", "-y"])
        .env("HOME", &f.home)
        .env("USERPROFILE", &f.home)
        .env("AGENTFLARE_HOME_OVERRIDE", &f.home)
        .env("AGENTFLARE_NO_INTERACTIVE", "1")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        !has_branch(&f.repo, "done"),
        "a loose merged branch still goes"
    );
    assert!(
        has_branch(&f.repo, "feature-x"),
        "the main checkout's branch is protected"
    );
    assert!(
        wt.exists(),
        "the worktree holding the current directory stays"
    );
    assert!(has_branch(&f.repo, "task/9-x"));
}

// Review finding 1, second half: state that belongs to the main checkout
// (orphaned worktree dirs, item state) is still found from a linked worktree.
#[test]
fn from_a_linked_worktree_the_main_checkout_is_what_gets_scanned() {
    let f = fixture();
    let wt = f.repo.join(".worktrees/task/9");
    git(
        &f.repo,
        &["worktree", "add", "-b", "task/9-x", wt.to_str().unwrap()],
    );
    let orphan = f.repo.join(".worktrees/task/12");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join(".git"), "gitdir: /nonexistent/worktrees/12\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_agentflare"))
        .current_dir(&wt)
        .args(["clean", "--dry-run", "--json"])
        .env("HOME", &f.home)
        .env("USERPROFILE", &f.home)
        .env("AGENTFLARE_HOME_OVERRIDE", &f.home)
        .env("AGENTFLARE_NO_INTERACTIVE", "1")
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", stdout(&out)));
    let ids: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["id"].as_str())
        .collect();
    assert!(ids.contains(&"orphan:12"), "{ids:?}");
}
