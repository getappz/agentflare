//! Integration tests: spawn the actual compiled shim binary against a real
//! temp repo, same as a caller on PATH would invoke it. `AGENTFLARE_HOME_OVERRIDE`
//! keeps the audit log out of the developer's real home directory.
//!
//! Test fixture setup goes through `flare_git_core::shell` (not a bare
//! `Command::new("git")`) so it resolves the real git binary the same
//! safe way the shim itself does -- a raw `Command::new("git")` here would
//! hit the exact self-resolution hazard `shell::git_binary` exists to
//! avoid, since this test binary also runs with the shared cargo
//! target/debug dir (where the shim's own `git.exe` lives) on PATH.

use std::path::Path;
use std::process::{Command, Output};

fn init_repo() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path();
    flare_git_core::shell::run_in(path, &["init", "-b", "master"]).unwrap();
    flare_git_core::shell::run_in(path, &["config", "user.email", "test@test.com"]).unwrap();
    flare_git_core::shell::run_in(path, &["config", "user.name", "Test"]).unwrap();
    flare_git_core::shell::run_in(path, &["commit", "--allow-empty", "-m", "initial"]).unwrap();
    dir
}

/// Runs the compiled shim binary (built by this crate as `git`/`git.exe`)
/// against `repo`, with a scratch `AGENTFLARE_HOME_OVERRIDE` so the audit
/// log doesn't land in the developer's real `~/.agentflare/`.
fn shim(repo: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_git"))
        .args(args)
        .current_dir(repo)
        .env("AGENTFLARE_HOME_OVERRIDE", home)
        .output()
        .unwrap()
}

#[test]
fn read_only_command_passes_through_and_succeeds() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    let out = shim(repo.path(), home.path(), &["status"]);
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn checkout_to_protected_branch_is_denied_and_real_git_never_runs() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    assert!(flare_git_core::shell::run_in_ok(
        repo.path(),
        &["checkout", "-b", "feature/x"]
    ));

    let out = shim(repo.path(), home.path(), &["checkout", "master"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("denied"), "{stderr}");

    // Real git must never have run -- still on feature/x.
    let branch = flare_git_core::shell::run_in(repo.path(), &["branch", "--show-current"]).unwrap();
    assert_eq!(branch.trim(), "feature/x");
}

#[test]
fn unrecognized_subcommand_is_denied() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    let out = shim(repo.path(), home.path(), &["some-made-up-subcommand"]);
    assert!(!out.status.success());
}

#[test]
fn escape_hatch_flags_are_denied() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    let out = shim(repo.path(), home.path(), &["-C", "/tmp", "status"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("denied"), "{stderr}");
}

#[test]
fn outside_a_git_repo_passes_through() {
    let dir = tempfile::TempDir::new().unwrap();
    let home = tempfile::TempDir::new().unwrap();
    let out = shim(dir.path(), home.path(), &["--version"]);
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn bypass_env_var_skips_classification_even_for_a_denied_command() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_git"))
        .args(["some-made-up-subcommand"])
        .current_dir(repo.path())
        .env("AGENTFLARE_HOME_OVERRIDE", home.path())
        .env("AGENTFLARE_GIT_BYPASS", "1")
        .output()
        .unwrap();
    // Real git also rejects the made-up subcommand, but for a DIFFERENT
    // reason (unknown git command, not "denied by the shim") -- assert on
    // stderr content, not just a nonzero exit, so this test would fail if
    // bypass silently started denying again.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("denied"), "{stderr}");
}

#[test]
fn denied_command_is_logged_to_the_audit_log() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    let out = shim(repo.path(), home.path(), &["some-made-up-subcommand"]);
    assert!(!out.status.success());

    let audit_log = home.path().join(".agentflare").join("audit").join("git.jsonl");
    let content = std::fs::read_to_string(&audit_log).expect("audit log must exist");
    assert!(content.contains("some-made-up-subcommand"), "{content}");
    assert!(content.contains("Deny"), "{content}");
}
