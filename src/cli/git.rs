//! `agentflare git install-hooks` — installs the shared branch-protection
//! git hooks (pre-commit / pre-push) into the current repository.
//!
//! The canonical hook scripts live in `~/.agentflare/githooks/` (populated by
//! this same command on first run, and reusable across every project). Each
//! invocation copies them into `<repo>/.githooks/` and points the repo's
//! `core.hooksPath` at that directory, so the guard is reproducible across
//! clones and applies to every git client (agent Bash, human CLI, CI) — not
//! just tool calls that route through the agent's PreToolUse hook.
//!
//! Why a git hook and not (only) the PreToolUse branch guard in
//! `src/hook_redirect.rs`: that guard only watches file-write tools
//! (`Write`/`Edit`/`ctx_patch`/...), so a `git commit`/`git push` issued
//! through a Bash/shell tool slips past it. A native git hook is the
//! shell-agnostic enforcement boundary. See item #132 follow-up.

use crate::paths::home;
use clap::{Args, Subcommand};
use std::fs;
use std::path::PathBuf;

#[derive(Args)]
pub struct GitArgs {
    #[command(subcommand)]
    pub command: GitCommand,
}

#[derive(Subcommand)]
pub enum GitCommand {
    /// Install branch-protection pre-commit/pre-push hooks into this repo.
    InstallHooks(InstallHooksArgs),
}

#[derive(Args)]
pub struct InstallHooksArgs {
    /// Skip the confirmation prompt (for non-interactive/scripted use).
    #[arg(long)]
    pub yes: bool,
}

/// Canonical location: `~/.agentflare/githooks/`.
fn shared_hooks_dir() -> PathBuf {
    home().join(".agentflare").join("githooks")
}

/// The hook scripts embedded as the canonical source of truth. Written into
/// `~/.agentflare/githooks/` on first `install-hooks`, so the shared location
/// is self-bootstrapping and survives repo checkouts.
const PRE_COMMIT: &str = include_str!("../../.githooks/pre-commit");
const PRE_PUSH: &str = include_str!("../../.githooks/pre-push");

fn ensure_shared_templates() -> std::io::Result<()> {
    let dir = shared_hooks_dir();
    fs::create_dir_all(&dir)?;
    let pc = dir.join("pre-commit");
    if !pc.exists() {
        fs::write(&pc, PRE_COMMIT)?;
    }
    let pp = dir.join("pre-push");
    if !pp.exists() {
        fs::write(&pp, PRE_PUSH)?;
    }
    Ok(())
}

pub fn run(args: GitArgs) {
    match args.command {
        GitCommand::InstallHooks(opts) => install_hooks(opts),
    }
}

fn install_hooks(opts: InstallHooksArgs) {
    let repo_root = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("agentflare git install-hooks: cannot resolve cwd: {e}");
            return;
        }
    };

    // Sanity: must be inside a git repo.
    if !repo_root.join(".git").exists()
        && run_git(&repo_root, &["rev-parse", "--git-dir"]).is_none()
    {
        eprintln!("agentflare git install-hooks: not a git repository (run inside a repo root)");
        return;
    }

    if let Err(e) = ensure_shared_templates() {
        eprintln!("agentflare git install-hooks: cannot write shared templates: {e}");
        return;
    }

    let local_dir = repo_root.join(".githooks");
    if let Err(e) = fs::create_dir_all(&local_dir) {
        eprintln!("agentflare git install-hooks: cannot create {local_dir:?}: {e}");
        return;
    }

    let mut changed = false;
    for name in ["pre-commit", "pre-push"] {
        let src = shared_hooks_dir().join(name);
        let dst = local_dir.join(name);
        match fs::copy(&src, &dst) {
            Ok(_) => {
                // Git requires the hook to be executable. On Unix the copied
                // file keeps the shared template's mode (0600 from a fresh
                // write), so make it user-executable. On Windows git runs
                // hooks through its bundled sh and ignores the bit, but
                // setting it is harmless and keeps the repo portable.
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = fs::set_permissions(&dst, fs::Permissions::from_mode(0o755));
                }
                println!("  ok    .githooks/{name}");
                changed = true;
            }
            Err(e) => {
                eprintln!("  fail  copying {name}: {e}");
                return;
            }
        }
    }

    // Point the repo at the local .githooks dir (relative, so it survives
    // clone/move). `git config` is run via the shell-free helper below.
    set_hooks_path(&repo_root, ".githooks");
    println!("  ok    core.hooksPath = .githooks");

    if changed {
        println!(
            "\nBranch-protection hooks installed. Direct commits/pushes to the \
             default branch are now blocked for every git client in this repo."
        );
        let _ = opts;
    }
}

fn run_git(repo: &std::path::Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

fn set_hooks_path(repo: &std::path::Path, path: &str) {
    let _ = std::process::Command::new("git")
        .args(["config", "core.hooksPath", path])
        .current_dir(repo)
        .output();
}
