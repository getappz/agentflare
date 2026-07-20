//! `agentflare git` -- git-related CLI surface: installing the shared
//! branch-protection hooks (pre-commit / pre-push / prepare-commit-msg /
//! reference-transaction) into a repo, installing/uninstalling the
//! flare-git-shim PATH shim, and the recovery-snapshot commands
//! (`snapshot list/restore/prune`) that make `flare_git_core::snapshot`'s
//! automatic pre-destructive snapshots actually usable.
//!
//! Why a git hook and not (only) the PreToolUse branch guard in
//! `src/hook_redirect.rs`: that guard only watches file-write tools
//! (`Write`/`Edit`/`ctx_patch`/...), so a `git commit`/`git push` issued
//! through a Bash/shell tool slips past it. A native git hook is the
//! shell-agnostic enforcement boundary. See item #132 follow-up.

use crate::paths::home;
use clap::{Args, Subcommand};
use flare_git_core::{audit, branch, provenance, snapshot};
use std::fs;
use std::io::Read as _;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct GitArgs {
    #[command(subcommand)]
    pub command: GitCommand,
}

#[derive(Subcommand)]
pub enum GitCommand {
    /// Install branch-protection/provenance git hooks into this repo.
    InstallHooks(InstallHooksArgs),
    /// Install the flare-git-shim binary (dogfooding/local use) as `git`
    /// on PATH, so every git invocation on this machine gets classified.
    InstallShim(InstallShimArgs),
    /// Remove the git shim installed by `install-shim`.
    UninstallShim,
    /// Recovery snapshots taken by the git shim before a destructive op.
    Snapshot(SnapshotArgs),
    /// (Internal, called by the `prepare-commit-msg` hook.) Appends
    /// provenance trailers to the commit message file.
    #[command(hide = true)]
    TrailerInject(TrailerInjectArgs),
    /// (Internal, called by the `reference-transaction` hook.) Reads ref
    /// updates from stdin and appends them to the backstop audit log.
    #[command(hide = true)]
    RefTransactionLog,
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

#[derive(Args)]
pub struct TrailerInjectArgs {
    /// Path to the commit-message file (`prepare-commit-msg`'s `$1`).
    pub msg_file: PathBuf,
}

#[derive(Args)]
pub struct SnapshotArgs {
    #[command(subcommand)]
    pub command: SnapshotCommand,
}

#[derive(Subcommand)]
pub enum SnapshotCommand {
    /// List recovery snapshots for this repo, newest first.
    List,
    /// Restore a snapshot's files into the working tree. Non-destructive:
    /// files created after the snapshot are left in place, never deleted.
    Restore(SnapshotRestoreArgs),
    /// Delete all but the most recent snapshots.
    Prune(SnapshotPruneArgs),
}

#[derive(Args)]
pub struct SnapshotRestoreArgs {
    /// Snapshot id (a commit sha, or any unambiguous prefix of one) to
    /// restore. Omit to use the only snapshot, or the newest with --yes.
    pub id: Option<String>,
    /// Skip the confirmation required when omitting `id` with more than
    /// one snapshot present, to pick the newest non-interactively.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Args)]
pub struct SnapshotPruneArgs {
    /// Number of most-recent snapshots to keep.
    #[arg(long, default_value_t = 5)]
    pub keep: usize,
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
const PREPARE_COMMIT_MSG: &str = include_str!("../../.githooks/prepare-commit-msg");
const REFERENCE_TRANSACTION: &str = include_str!("../../.githooks/reference-transaction");

/// Every hook this command installs, in (filename, embedded template) pairs.
const HOOKS: &[(&str, &str)] = &[
    ("pre-commit", PRE_COMMIT),
    ("pre-push", PRE_PUSH),
    ("prepare-commit-msg", PREPARE_COMMIT_MSG),
    ("reference-transaction", REFERENCE_TRANSACTION),
];

fn ensure_shared_templates() -> std::io::Result<()> {
    let dir = shared_hooks_dir();
    fs::create_dir_all(&dir)?;
    for (name, template) in HOOKS {
        let path = dir.join(name);
        if !path.exists() {
            fs::write(&path, template)?;
        }
    }
    Ok(())
}

pub fn run(args: GitArgs) {
    match args.command {
        GitCommand::InstallHooks(opts) => install_hooks(opts),
        GitCommand::InstallShim(opts) => install_shim(opts),
        GitCommand::UninstallShim => uninstall_shim(),
        GitCommand::Snapshot(opts) => snapshot_cmd(opts),
        GitCommand::TrailerInject(opts) => trailer_inject(&opts.msg_file),
        GitCommand::RefTransactionLog => ref_transaction_log(),
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
        crate::ui::error(&format!(
            "agentflare git install-shim: cannot create {dir:?}: {e}"
        ));
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
        Err(e) => crate::ui::error(&format!(
            "agentflare git install-shim: could not update PATH: {e}"
        )),
    }

    println!(
        "
Once your PATH refreshes, every `git` command on this machine is classified by the agentflare git shim. Escape hatches: AGENTFLARE_GIT_BYPASS=1 (one-shot), AGENTFLARE_GIT_BYPASS_AGENT=<name>, AGENTFLARE_GIT_BYPASS_UNTIL=<unix-epoch>. Remove entirely with `agentflare git uninstall-shim`."
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
        Err(e) => crate::ui::error(&format!(
            "agentflare git uninstall-shim: cannot remove {dest:?}: {e}"
        )),
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
fn ensure_on_path(dir: &Path) -> Result<bool, String> {
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
    let already_present = current.split(';').any(|p| {
        p.trim_end_matches('\u{5c}')
            .eq_ignore_ascii_case(dir_str.trim_end_matches('\u{5c}'))
    });
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
fn ensure_on_path(_dir: &Path) -> Result<bool, String> {
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
    if branch::repo_toplevel(&repo_root).is_none() {
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
    for (name, _) in HOOKS {
        let src = shared_hooks_dir().join(name);
        let dst = local_dir.join(name);
        match fs::copy(&src, &dst) {
            Ok(_) => {
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

    flare_git_core::shell::run_in(&repo_root, &["config", "core.hooksPath", ".githooks"]).ok();
    crate::ui::success("core.hooksPath = .githooks");

    if changed {
        println!(
            "\nBranch-protection hooks installed. Direct commits/pushes to the \
             default branch are now blocked for every git client in this repo. \
             Commits are also stamped with provenance trailers, and every ref \
             move is journaled to ~/.agentflare/audit/git-refs.jsonl."
        );
        let _ = opts;
    }
}

/// Resolves the git repo root from the current working directory, printing
/// a consistent error and returning `None` if we're not inside one.
fn resolve_repo_root(command_name: &str) -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let root = branch::repo_toplevel(&cwd);
    if root.is_none() {
        crate::ui::error(&format!(
            "agentflare git {command_name}: not a git repository (run inside a repo root)"
        ));
    }
    root
}

fn snapshot_cmd(args: SnapshotArgs) {
    let Some(repo_root) = resolve_repo_root("snapshot") else {
        return;
    };
    match args.command {
        SnapshotCommand::List => snapshot_list(&repo_root),
        SnapshotCommand::Restore(opts) => snapshot_restore(&repo_root, &opts),
        SnapshotCommand::Prune(opts) => snapshot_prune(&repo_root, &opts),
    }
}

fn snapshot_list(repo_root: &Path) {
    let snaps = snapshot::list(repo_root);
    if snaps.is_empty() {
        println!("No snapshots for this repo.");
        return;
    }
    for s in snaps {
        let short_id = &s.id.0[..s.id.0.len().min(12)];
        println!("{short_id}  {}  {}", s.committer_date, s.reason);
    }
}

fn snapshot_restore(repo_root: &Path, opts: &SnapshotRestoreArgs) {
    let snaps = snapshot::list(repo_root);
    let target = match &opts.id {
        Some(id) => snaps.iter().find(|s| s.id.0.starts_with(id.as_str())),
        None => match snaps.len() {
            0 => None,
            1 => snaps.first(),
            _ if opts.yes => snaps.first(),
            _ => {
                crate::ui::error(
                    "agentflare git snapshot restore: multiple snapshots exist -- pass an id, or --yes to use the newest",
                );
                return;
            }
        },
    };
    let Some(meta) = target else {
        crate::ui::error("agentflare git snapshot restore: no matching snapshot found");
        return;
    };
    match snapshot::restore(repo_root, &meta.id) {
        Ok(()) => crate::ui::success(&format!(
            "restored snapshot {} ({})",
            &meta.id.0[..meta.id.0.len().min(12)],
            meta.reason
        )),
        Err(e) => crate::ui::error(&format!("agentflare git snapshot restore: {e}")),
    }
}

fn snapshot_prune(repo_root: &Path, opts: &SnapshotPruneArgs) {
    match snapshot::prune(repo_root, opts.keep) {
        Ok(()) => crate::ui::success(&format!("pruned snapshots, kept {} most recent", opts.keep)),
        Err(e) => crate::ui::error(&format!("agentflare git snapshot prune: {e}")),
    }
}

/// `agentflare git trailer-inject <msg-file>` -- called by the
/// `prepare-commit-msg` hook. Fail-open: any error leaves the message file
/// untouched rather than blocking the commit.
fn trailer_inject(msg_file: &Path) {
    let Some(repo_root) = branch::repo_toplevel(&std::env::current_dir().unwrap_or_default())
    else {
        return;
    };
    let Ok(original) = fs::read_to_string(msg_file) else {
        return;
    };
    let trailers = provenance::build_trailers(&repo_root);
    let updated = provenance::append_trailers(&original, &trailers);
    if updated != original {
        let _ = fs::write(msg_file, updated);
    }
}

/// `agentflare git ref-transaction-log` -- called by the
/// `reference-transaction` hook with ref-update lines
/// (`<old-oid> <new-oid> <refname>`) on stdin. Fail-open: this only
/// observes, it can never affect the underlying git operation either way.
fn ref_transaction_log() {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return;
    }
    let transactions: Vec<audit::RefTransaction> = input
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some(audit::RefTransaction {
                old: parts.next()?.to_string(),
                new: parts.next()?.to_string(),
                refname: parts.next()?.to_string(),
            })
        })
        .collect();
    if transactions.is_empty() {
        return;
    }
    let repo_root = branch::repo_toplevel(&std::env::current_dir().unwrap_or_default());
    let agent = repo_root
        .as_deref()
        .and_then(|root| provenance::build_trailers(root).agent);
    let event = audit::RefTransactionEvent {
        agent,
        transactions,
    };
    if let Some(path) = audit::default_path("git-refs.jsonl") {
        let _ = audit::log_event(&path, &event);
    }
}
