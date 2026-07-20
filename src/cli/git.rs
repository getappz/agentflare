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
    /// Install the flare-git-shim binary (dogfooding/local use) as `git`
    /// on PATH, so every git invocation on this machine gets classified.
    InstallShim(InstallShimArgs),
    /// Remove the git shim installed by `install-shim`.
    UninstallShim,
}

#[derive(Args)]
pub struct InstallShimArgs {
    /// Path to a compiled flare-git-shim binary (its `[[bin]] name = "git"`
    /// target) to install. No auto-discovery yet -- this is a dogfooding
    /// aid, not the production release path (that will bundle the shim
    /// alongside the main binary via install.sh/install.ps1).
    #[arg(long)]
    pub binary: PathBuf,
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
        GitCommand::InstallShim(opts) => install_shim(opts),
        GitCommand::UninstallShim => uninstall_shim(),
    }
}

/// Canonical location: `~/.agentflare/shims/` -- same directory
/// `agentflare-shim` (item #227's lean-ctx PATH shim) already uses, so
/// there's one PATH entry to manage, not several.
fn shims_dir() -> PathBuf {
    home().join(".agentflare").join("shims")
}

fn shim_dest_name() -> &'static str {
    if cfg!(windows) { "git.exe" } else { "git" }
}

fn install_shim(opts: InstallShimArgs) {
    let dir = shims_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        crate::ui::error(&format!("agentflare git install-shim: cannot create {dir:?}: {e}"));
        return;
    }
    let dest = dir.join(shim_dest_name());
    if let Err(e) = fs::copy(&opts.binary, &dest) {
        crate::ui::error(&format!(
            "agentflare git install-shim: cannot copy {:?} to {dest:?}: {e}",
            opts.binary
        ));
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dest, fs::Permissions::from_mode(0o755));
    }
    crate::ui::success(&format!("installed git shim -> {}", dest.display()));

    match ensure_on_path(&dir) {
        Ok(true) => crate::ui::success(&format!(
            "added {} to your User PATH -- restart your terminal/IDE to pick it up",
            dir.display()
        )),
        Ok(false) => crate::ui::success(&format!("{} already on PATH", dir.display())),
        Err(e) => crate::ui::error(&format!("agentflare git install-shim: could not update PATH: {e}")),
    }

    println!(
        "
Once your PATH refreshes, every `git` command on this machine is classified by the agentflare git shim. Escape hatch: set AGENTFLARE_GIT_BYPASS=1 to skip classification for a command/session without uninstalling. Remove entirely with `agentflare git uninstall-shim`."
    );
}

fn uninstall_shim() {
    let dest = shims_dir().join(shim_dest_name());
    if !dest.exists() {
        crate::ui::success("git shim was not installed");
        return;
    }
    match fs::remove_file(&dest) {
        Ok(()) => crate::ui::success(&format!("removed {}", dest.display())),
        Err(e) => crate::ui::error(&format!("agentflare git uninstall-shim: cannot remove {dest:?}: {e}")),
    }
    // Deliberately leaves the shims dir on PATH -- other shims (e.g. the
    // lean-ctx one) may still live there; removing just this binary is
    // enough to fully restore normal git behavior.
}

/// Prepends `dir` to the current user's persistent PATH (Windows: the
/// `User` environment scope via PowerShell, since it needs to survive
/// across terminal sessions and there's no portable non-shelling way to
/// do this without an extra crate). Returns `Ok(true)` if PATH was
/// changed, `Ok(false)` if `dir` was already present.
#[cfg(windows)]
fn ensure_on_path(dir: &std::path::Path) -> Result<bool, String> {
    let dir_str = dir.to_string_lossy().to_string();
    let get = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-Command",
            "[Environment]::GetEnvironmentVariable('PATH','User')",
        ])
        .output()
        .map_err(|e| e.to_string())?;
    let current = String::from_utf8_lossy(&get.stdout).trim().to_string();
    let already_present = current
        .split(';')
        .any(|p| p.trim_end_matches('\\').eq_ignore_ascii_case(dir_str.trim_end_matches('\\')));
    if already_present {
        return Ok(false);
    }
    let new_path = if current.is_empty() {
        dir_str.clone()
    } else {
        format!("{dir_str};{current}")
    };
    let set_script = format!(
        "[Environment]::SetEnvironmentVariable('PATH', '{}', 'User')",
        new_path.replace('\'', "''")
    );
    let set = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", &set_script])
        .status()
        .map_err(|e| e.to_string())?;
    if !set.success() {
        return Err("powershell SetEnvironmentVariable failed".to_string());
    }
    Ok(true)
}

#[cfg(not(windows))]
fn ensure_on_path(_dir: &std::path::Path) -> Result<bool, String> {
    // Not needed for this dogfooding session (Windows-only machine); the
    // real install.sh wiring will handle shell-profile PATH export the
    // same way it already does for the main binary's install dir.
    Ok(false)
}

fn install_hooks(opts: InstallHooksArgs) {
    let repo_root = match std::env::current_dir() {
        Ok(d) => d,
        Err(e) => {
            crate::ui::error(&format!(
                "agentflare git install-hooks: cannot resolve cwd: {e}"
            ));
            return;
        }
    };

    // Sanity: must be inside a git repo.
    if !repo_root.join(".git").exists()
        && run_git(&repo_root, &["rev-parse", "--git-dir"]).is_none()
    {
        crate::ui::error(
            "agentflare git install-hooks: not a git repository (run inside a repo root)",
        );
        return;
    }

    if let Err(e) = ensure_shared_templates() {
        crate::ui::error(&format!(
            "agentflare git install-hooks: cannot write shared templates: {e}"
        ));
        return;
    }

    let local_dir = repo_root.join(".githooks");
    if let Err(e) = fs::create_dir_all(&local_dir) {
        crate::ui::error(&format!(
            "agentflare git install-hooks: cannot create {local_dir:?}: {e}"
        ));
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
                crate::ui::success(&format!(".githooks/{name}"));
                changed = true;
            }
            Err(e) => {
                crate::ui::error(&format!("copying {name}: {e}"));
                return;
            }
        }
    }

    // Point the repo at the local .githooks dir (relative, so it survives
    // clone/move). `git config` is run via the shell-free helper below.
    set_hooks_path(&repo_root, ".githooks");
    crate::ui::success("core.hooksPath = .githooks");

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
