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
use flare_git_core::{audit, branch, classify, provenance, scope, shell, snapshot, worktree};
use std::collections::HashSet;
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
    /// (Internal, called by flare-git-shim.) Checks a commit/push against
    /// live claim scopes -- see item #234.
    #[command(hide = true)]
    ScopeCheck(ScopeCheckArgs),
    /// Preview or prune orphaned worktree directories.
    Audit(WorktreeAuditArgs),
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
pub struct ScopeCheckArgs {
    /// The subcommand being checked -- "commit" or "push".
    #[arg(long)]
    pub subcommand: String,
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

#[derive(Args)]
pub struct WorktreeAuditArgs {
    #[command(subcommand)]
    pub command: WorktreeAuditCommand,
}

#[derive(Subcommand)]
pub enum WorktreeAuditCommand {
    /// List orphaned worktree directories.
    Preview,
    /// Remove orphaned worktree directories (snapshots taken first).
    Prune(WorktreeAuditPruneArgs),
}

#[derive(Args)]
pub struct WorktreeAuditPruneArgs {
    /// Names of worktrees to prune (as shown by preview). Pass --all to
    /// prune every orphan.
    #[arg(required_unless_present = "all")]
    pub names: Vec<String>,
    /// Prune all orphaned worktrees.
    #[arg(long, short)]
    pub all: bool,
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
        GitCommand::ScopeCheck(opts) => scope_check(&opts.subcommand),
        GitCommand::Audit(opts) => worktree_audit_cmd(opts),
    }
}

/// Canonical location: `~/.agentflare/shims/` -- same directory
/// `agentflare-shim` (item #227's lean-ctx PATH shim) already uses, so
/// there's one PATH entry to manage, not several.
pub(crate) fn shims_dir() -> PathBuf {
    home().join(".agentflare").join("shims")
}

pub(crate) fn shim_dest_name() -> &'static str {
    if cfg!(windows) { "git.exe" } else { "git" }
}

/// Copies `binary` to `dir`/[`shim_dest_name`], chmod +x on Unix. Shared by
/// the explicit `install-shim` CLI command and `shim_install`'s auto-install
/// during `init`.
pub(crate) fn install_git_shim_binary(dir: &Path, binary: &Path) -> std::io::Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let dest = dir.join(shim_dest_name());
    fs::copy(binary, &dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o755))?;
    }
    Ok(dest)
}

fn install_shim(opts: InstallShimArgs) {
    let dir = shims_dir();
    let dest = match install_git_shim_binary(&dir, &opts.binary) {
        Ok(dest) => dest,
        Err(e) => {
            crate::ui::error(&format!(
                "agentflare git install-shim: cannot install {:?} to {dir:?}: {e}",
                opts.binary
            ));
            return;
        }
    };
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
pub(crate) fn ensure_on_path(dir: &Path) -> Result<bool, String> {
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
pub(crate) fn ensure_on_path(_dir: &Path) -> Result<bool, String> {
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

fn worktree_audit_cmd(args: WorktreeAuditArgs) {
    let Some(repo_root) = resolve_repo_root("audit") else {
        return;
    };
    match args.command {
        WorktreeAuditCommand::Preview => worktree_audit_preview(&repo_root),
        WorktreeAuditCommand::Prune(opts) => worktree_audit_prune(&repo_root, &opts),
    }
}

fn worktree_audit_preview(repo_root: &Path) {
    let claimed = claimed_sequence_ids(repo_root);
    let orphans = worktree::audit_orphans(repo_root, Some(&claimed));
    if orphans.is_empty() {
        println!("No orphaned worktrees found.");
        return;
    }
    for o in &orphans {
        let size_mb = o.size_bytes as f64 / 1_048_576.0;
        let seq = o
            .sequence_id
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".into());
        let flag = if o.has_broken_gitdir {
            " [broken .git]"
        } else {
            ""
        };
        println!("{seq:>6}  {size_mb:>8.2} MB  {}{flag}", o.name);
    }
    let total_mb = orphans.iter().map(|o| o.size_bytes).sum::<u64>() as f64 / 1_048_576.0;
    println!("────────────────────────────");
    println!("{} orphan(s), {total_mb:.2} MB total", orphans.len());
}

fn worktree_audit_prune(repo_root: &Path, opts: &WorktreeAuditPruneArgs) {
    let claimed = claimed_sequence_ids(repo_root);
    let orphans = worktree::audit_orphans(repo_root, Some(&claimed));
    let to_prune: Vec<String> = if opts.all {
        orphans.into_iter().map(|o| o.name).collect()
    } else {
        let set: HashSet<&str> = opts.names.iter().map(|s| s.as_str()).collect();
        orphans
            .into_iter()
            .filter(|o| set.contains(o.name.as_str()))
            .map(|o| o.name)
            .collect()
    };
    if to_prune.is_empty() {
        println!("No matching orphans to prune.");
        return;
    }
    let deleted = worktree::gc_orphans(repo_root, &to_prune);
    for name in &deleted {
        println!("pruned: {}", name);
    }
    if deleted.len() < to_prune.len() {
        for name in to_prune.iter().filter(|n| !deleted.contains(n)) {
            eprintln!("warning: failed to prune '{}'", name);
        }
    }
}

/// Resolves the git repo root from the current working directory, printing
/// a consistent error and returning `None` if we're not inside one.
/// Build set of claimed item sequence_ids from the DB.
fn claimed_sequence_ids(_repo_root: &Path) -> HashSet<String> {
    let conn = match crate::db::open() {
        Ok(c) => c,
        Err(_) => return HashSet::new(),
    };
    let now = db_kit::ids::now();
    let ttl = agentflare_backend::claim::default_ttl_secs();
    let claimed_ids: HashSet<String> = match agentflare_backend::claim::list_active(&conn, now, ttl)
    {
        Ok(c) => c.into_iter().collect(),
        Err(_) => return HashSet::new(),
    };
    // Query all non-deleted items across all projects whose id appears
    // in the active-claim set, collect their sequence_ids.
    let mut stmt = match conn.prepare("SELECT id, sequence_id FROM items WHERE deleted_at IS NULL")
    {
        Ok(s) => s,
        Err(_) => return HashSet::new(),
    };
    let rows: Vec<(String, i64)> =
        match stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))) {
            Ok(r) => r.filter_map(|r| r.ok()).collect(),
            Err(_) => return HashSet::new(),
        };
    rows.into_iter()
        .filter(|(id, _)| claimed_ids.contains(id))
        .map(|(_, seq)| seq.to_string())
        .collect()
}

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

#[derive(serde::Serialize)]
struct ScopeCheckResult {
    deny: bool,
    reason: Option<String>,
}

fn scope_pass() -> ScopeCheckResult {
    ScopeCheckResult {
        deny: false,
        reason: None,
    }
}

fn scope_deny(reason: String) -> ScopeCheckResult {
    ScopeCheckResult {
        deny: true,
        reason: Some(reason),
    }
}

/// `agentflare git scope-check --subcommand commit|push` -- called by
/// flare-git-shim before letting a commit/push through, to enforce item
/// #234's claim path-scopes (data the shim itself has no DB access to).
/// Always prints one line of JSON to stdout and exits 0 -- denial lives IN
/// the JSON (`deny`/`reason`), not the exit code, so the shim can tell
/// "scope-check ran and said no" apart from "scope-check itself failed to
/// run at all" (the latter is the shim's fail-closed case, per this
/// feature's spec -- unlike this crate's usual fail-open default).
fn scope_check(subcommand: &str) {
    let result = run_scope_check(subcommand);
    let json = serde_json::to_string(&result).unwrap_or_else(|_| {
        r#"{"deny":true,"reason":"internal error serializing scope-check result"}"#.to_string()
    });
    println!("{json}");
}

fn run_scope_check(subcommand: &str) -> ScopeCheckResult {
    // Scope enforcement only applies to agent-driven invocations, mirroring
    // `flare-git-shim`'s existing canonical-detach guard -- interactive
    // human use is never affected.
    if !classify::agent_invocation_detected() {
        return scope_pass();
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    let Some(repo_root) = branch::repo_toplevel(&cwd) else {
        return scope_pass(); // not in a repo -- nothing to check
    };
    let Some(repo) = crate::claims::resolve_repo(None) else {
        return scope_pass(); // no resolvable repo key -> no claims possible
    };
    let conn = match crate::db::open() {
        Ok(c) => c,
        Err(e) => return scope_deny(format!("cannot open claim ledger: {e}")),
    };
    let now = crate::claims::now();
    let ttl = crate::claims::ttl_secs();
    let live = match crate::claims::list(&conn, Some(&repo), false, now, ttl) {
        Ok(v) => v,
        Err(e) => return scope_deny(format!("cannot query live claims: {e}")),
    };
    if live.is_empty() {
        return scope_pass();
    }

    let owner = crate::claims::owner_id();
    let agent = crate::claims::agent_of(&owner);
    let own_target = live
        .iter()
        .find(|c| crate::claims::agent_of(&c.owner) == agent)
        .map(|c| c.target.clone());
    let others: Vec<scope::ClaimScope> = live
        .iter()
        .filter(|c| crate::claims::agent_of(&c.owner) != agent)
        .map(|c| scope::ClaimScope {
            target: c.target.clone(),
            owner: c.owner.clone(),
            scopes: c.scope.clone(),
        })
        .collect();

    let changed = changed_paths(&repo_root, subcommand);
    let in_worktree = branch::is_linked_worktree(&repo_root);
    match scope::classify_scopes(&changed, own_target.as_deref(), in_worktree, &others) {
        scope::ScopeVerdict::Overlapping {
            owner,
            target,
            scope,
        } => scope_deny(format!(
            "this touches path(s) inside claim '{target}' (owner {owner}, scope '{scope}') -- work inside that claim's own worktree, or coordinate with {owner}."
        )),
        scope::ScopeVerdict::OutOfTree { target } => scope_deny(format!(
            "you hold claim '{target}' -- do this work in its isolated worktree, not the canonical checkout (see `git worktree add`)."
        )),
        scope::ScopeVerdict::Clear | scope::ScopeVerdict::Related => scope_pass(),
    }
}

/// Changed paths for the mutation about to happen -- staged paths for
/// `commit` (unioned with the working-tree diff, since `git commit -a`/
/// `--all` implicitly stages+commits tracked modifications without a prior
/// `git add` -- checking `--cached` alone would let those paths bypass
/// scope enforcement entirely), paths diffed against the default branch for
/// `push`. A v1 simplification for `push`: diffs current-vs-default rather
/// than parsing the actual push refspec across the CLI subprocess boundary.
/// An unreadable diff yields no changed paths (nothing to enforce), matching
/// this crate's fail-open default for diff resolution specifically -- only
/// scope RESOLUTION errors (ledger/DB) are fail-closed, per the spec.
fn changed_paths(repo_root: &Path, subcommand: &str) -> Vec<String> {
    if subcommand == "commit" {
        let mut paths: Vec<String> = shell::run_in(repo_root, &["diff", "--cached", "--name-only"])
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default();
        paths.extend(
            shell::run_in(repo_root, &["diff", "--name-only"])
                .map(|s| s.lines().map(String::from).collect::<Vec<_>>())
                .unwrap_or_default(),
        );
        paths.sort();
        paths.dedup();
        return paths;
    }
    let range_args: Vec<String> = match subcommand {
        "push" => {
            let default_branch = branch::resolve_default_branch(repo_root);
            let current =
                branch::current_branch(repo_root).unwrap_or_else(|| default_branch.clone());
            let range = format!("{default_branch}...{current}");
            vec!["diff".to_string(), "--name-only".to_string(), range]
        }
        _ => return Vec::new(),
    };
    let args: Vec<&str> = range_args.iter().map(String::as_str).collect();
    shell::run_in(repo_root, &args)
        .map(|s| s.lines().map(String::from).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run_git(dir.path(), &["init", "-q", "-b", "master"]);
        run_git(dir.path(), &["config", "user.email", "t@t"]);
        run_git(dir.path(), &["config", "user.name", "t"]);
        std::fs::write(dir.path().join("tracked.txt"), "v1\n").unwrap();
        run_git(dir.path(), &["add", "tracked.txt"]);
        run_git(dir.path(), &["commit", "-q", "-m", "seed"]);
        dir
    }

    #[test]
    fn changed_paths_for_commit_includes_unstaged_modifications_not_just_staged() {
        // Regression for the CodeRabbit-flagged bypass on PR #303: `git
        // commit -a`/`--all` implicitly stages+commits tracked
        // modifications without a prior `git add`, so checking only
        // `--cached` would miss them and let the change bypass scope
        // enforcement entirely.
        let repo = init_repo();
        std::fs::write(repo.path().join("tracked.txt"), "v2\n").unwrap();
        let paths = changed_paths(repo.path(), "commit");
        assert!(
            paths.iter().any(|p| p == "tracked.txt"),
            "unstaged modification must be included: {paths:?}"
        );
    }

    #[test]
    fn changed_paths_for_commit_dedupes_staged_and_unstaged() {
        let repo = init_repo();
        std::fs::write(repo.path().join("new.txt"), "x\n").unwrap();
        run_git(repo.path(), &["add", "new.txt"]);
        let paths = changed_paths(repo.path(), "commit");
        assert_eq!(
            paths.iter().filter(|p| *p == "new.txt").count(),
            1,
            "{paths:?}"
        );
    }

    #[test]
    fn changed_paths_for_commit_is_empty_when_clean() {
        let repo = init_repo();
        assert!(changed_paths(repo.path(), "commit").is_empty());
    }
}
