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
fn unrecognized_subcommand_passes_through_to_real_git() {
    // Fail-open: a subcommand this shim doesn't recognize is not denied by
    // the shim -- it's handed to real git unchanged, which then rejects it
    // for its OWN reason ("not a git command"), not "denied by the shim".
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    let out = shim(repo.path(), home.path(), &["some-made-up-subcommand"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("denied"), "{stderr}");
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
    assert!(flare_git_core::shell::run_in_ok(
        repo.path(),
        &["checkout", "-b", "feature/x"]
    ));
    let out = Command::new(env!("CARGO_BIN_EXE_git"))
        .args(["checkout", "master"])
        .current_dir(repo.path())
        .env("AGENTFLARE_HOME_OVERRIDE", home.path())
        .env("AGENTFLARE_GIT_BYPASS", "1")
        .output()
        .unwrap();
    // Without bypass this exact command is the explicit protected-branch
    // deny case (see checkout_to_protected_branch_is_denied_and_real_git_never_runs);
    // with bypass it must run unconditionally.
    assert!(out.status.success(), "{out:?}");
    let branch = flare_git_core::shell::run_in(repo.path(), &["branch", "--show-current"]).unwrap();
    assert_eq!(branch.trim(), "master");
}

#[test]
fn denied_command_is_logged_to_the_audit_log() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    assert!(flare_git_core::shell::run_in_ok(
        repo.path(),
        &["checkout", "-b", "feature/x"]
    ));
    let out = shim(repo.path(), home.path(), &["checkout", "master"]);
    assert!(!out.status.success());

    let audit_log = home.path().join(".agentflare").join("audit").join("git.jsonl");
    let content = std::fs::read_to_string(&audit_log).expect("audit log must exist");
    assert!(content.contains("checkout"), "{content}");
    assert!(content.contains("Deny"), "{content}");
}

#[test]
fn bypass_agent_env_var_bypasses_only_for_the_matching_agent() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    assert!(flare_git_core::shell::run_in_ok(
        repo.path(),
        &["checkout", "-b", "feature/x"]
    ));

    // Matching agent -- bypasses.
    let out = Command::new(env!("CARGO_BIN_EXE_git"))
        .args(["checkout", "master"])
        .current_dir(repo.path())
        .env("AGENTFLARE_HOME_OVERRIDE", home.path())
        .env("AGENTFLARE_AGENT", "claude-code")
        .env("AGENTFLARE_GIT_BYPASS_AGENT", "claude-code")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");

    // Back to feature/x, try again with a DIFFERENT agent -- must still deny.
    flare_git_core::shell::run_in(repo.path(), &["checkout", "feature/x"]).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_git"))
        .args(["checkout", "master"])
        .current_dir(repo.path())
        .env("AGENTFLARE_HOME_OVERRIDE", home.path())
        .env("AGENTFLARE_AGENT", "some-other-agent")
        .env("AGENTFLARE_GIT_BYPASS_AGENT", "claude-code")
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");
}

#[test]
fn bypass_until_env_var_respects_the_deadline() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    assert!(flare_git_core::shell::run_in_ok(
        repo.path(),
        &["checkout", "-b", "feature/x"]
    ));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Deadline in the past -- must still deny.
    let out = Command::new(env!("CARGO_BIN_EXE_git"))
        .args(["checkout", "master"])
        .current_dir(repo.path())
        .env("AGENTFLARE_HOME_OVERRIDE", home.path())
        .env("AGENTFLARE_GIT_BYPASS_UNTIL", (now - 60).to_string())
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");

    // Deadline in the future -- bypasses.
    let out = Command::new(env!("CARGO_BIN_EXE_git"))
        .args(["checkout", "master"])
        .current_dir(repo.path())
        .env("AGENTFLARE_HOME_OVERRIDE", home.path())
        .env("AGENTFLARE_GIT_BYPASS_UNTIL", (now + 3600).to_string())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn snapshots_disabled_env_var_skips_the_pre_destructive_snapshot() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    std::fs::write(repo.path().join("f.txt"), "dirty").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_git"))
        .args(["reset", "--hard"])
        .current_dir(repo.path())
        .env("AGENTFLARE_HOME_OVERRIDE", home.path())
        .env("AGENTFLARE_GIT_SNAPSHOTS", "0")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(
        flare_git_core::snapshot::list(repo.path()).is_empty(),
        "no snapshot should have been taken with AGENTFLARE_GIT_SNAPSHOTS=0"
    );
}

#[test]
fn snapshots_enabled_by_default_before_a_destructive_op() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    std::fs::write(repo.path().join("f.txt"), "dirty").unwrap();

    let out = shim(repo.path(), home.path(), &["reset", "--hard"]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        !flare_git_core::snapshot::list(repo.path()).is_empty(),
        "a snapshot should have been taken by default"
    );
}

#[test]
fn canonical_repo_detach_is_denied_for_agent_invocation_but_not_human() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    let sha = flare_git_core::shell::run_in(repo.path(), &["rev-parse", "HEAD"]).unwrap();

    // Agent-invoked (CLAUDECODE marker set) -- denied.
    let out = Command::new(env!("CARGO_BIN_EXE_git"))
        .args(["checkout", &sha])
        .current_dir(repo.path())
        .env("AGENTFLARE_HOME_OVERRIDE", home.path())
        .env("CLAUDECODE", "1")
        .output()
        .unwrap();
    assert!(!out.status.success(), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("canonical checkout"), "{stderr}");

    // No agent marker -- ordinary human usage, passes through. This test
    // process itself may be running under an agent-marked environment (it
    // is, under Claude Code -- CLAUDECODE=1), so explicitly strip every
    // agent marker rather than relying on ambient absence.
    let out = Command::new(env!("CARGO_BIN_EXE_git"))
        .args(["checkout", &sha])
        .current_dir(repo.path())
        .env("AGENTFLARE_HOME_OVERRIDE", home.path())
        .env_remove("CLAUDECODE")
        .env_remove("CURSOR_AGENT")
        .env_remove("CODEX_CLI_SESSION")
        .env_remove("GEMINI_SESSION")
        .env_remove("CODEBUDDY")
        .env_remove("AGENTFLARE_AGENT")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}

#[test]
fn canonical_repo_detach_allowed_with_escape_hatch() {
    let repo = init_repo();
    let home = tempfile::TempDir::new().unwrap();
    let sha = flare_git_core::shell::run_in(repo.path(), &["rev-parse", "HEAD"]).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_git"))
        .args(["checkout", &sha])
        .current_dir(repo.path())
        .env("AGENTFLARE_HOME_OVERRIDE", home.path())
        .env("CLAUDECODE", "1")
        .env("AGENTFLARE_GIT_ALLOW_CANONICAL_MUTATE", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
}
