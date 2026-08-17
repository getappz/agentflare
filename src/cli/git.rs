//! `agentflare git` -- git-related CLI surface: installing the shared
//! branch-protection hooks (pre-commit / pre-merge-commit / pre-push /
//! prepare-commit-msg / reference-transaction / post-commit) into a repo,
//! installing/uninstalling the flare-git-shim PATH shim, and the
//! recovery-snapshot commands
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
use flare_git_core::shell::BoundedLinesError;
use flare_git_core::{
    audit, branch, classify, doctor, provenance, scope, shell, snapshot, worktree,
};
use std::collections::{HashMap, HashSet};
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
    /// Health sweep over all claim worktrees (flare doctor).
    Doctor(DoctorArgs),
    /// Push the current branch, open (or find) its PR, then poll CI status
    /// -- the "ship it" macro. Requires a clean working tree (commit first);
    /// this never stages or commits for you.
    Ship(ShipArgs),
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

#[derive(Args)]
pub struct DoctorArgs {
    /// Output format: text, json, or markdown.
    #[arg(long, value_enum, default_value = "text")]
    pub format: DoctorFormat,
    /// Reclaim clean stale/orphaned/zombie worktrees.
    #[arg(long)]
    pub reclaim: bool,
    /// Force reclaim even dirty lanes (use with caution).
    #[arg(long)]
    pub force: bool,
    /// Staleness threshold in days.
    #[arg(long, default_value_t = 14)]
    pub staleness_days: u64,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum DoctorFormat {
    Text,
    Json,
    Markdown,
}

#[derive(Args)]
pub struct ShipArgs {
    /// Base branch to open the PR against. Defaults to the repo's resolved default branch.
    #[arg(long)]
    pub base: Option<String>,
    /// PR title. Defaults to the current branch's latest commit subject and
    /// must be a conventional-commit type (feat/fix/docs/...).
    #[arg(long)]
    pub title: Option<String>,
    /// PR body. Defaults to a bullet list of base..branch commit subjects.
    #[arg(long)]
    pub body: Option<String>,
    /// Skip polling CI status after the PR is open/found.
    #[arg(long)]
    pub no_wait: bool,
    /// Max seconds to poll CI before giving up (the PR is already open by then).
    #[arg(long, default_value_t = 300)]
    pub wait_secs: u64,
}

/// Canonical location: `~/.agentflare/githooks/`.
fn shared_hooks_dir() -> PathBuf {
    home().join(".agentflare").join("githooks")
}

/// The hook scripts embedded as the canonical source of truth. Written into
/// `~/.agentflare/githooks/` on first `install-hooks`, so the shared location
/// is self-bootstrapping and survives repo checkouts.
const PRE_COMMIT: &str = include_str!("../../.githooks/pre-commit");
// `pre-commit` alone does not fire for a merge commit -- git only invokes it
// for a plain `git commit`. `pre-merge-commit` is git's separate hook for
// that (githooks(5)); ours just execs `pre-commit` so there's one source of
// truth for what "direct commit to the default branch" means.
const PRE_MERGE_COMMIT: &str = include_str!("../../.githooks/pre-merge-commit");
const PRE_PUSH: &str = include_str!("../../.githooks/pre-push");
const PREPARE_COMMIT_MSG: &str = include_str!("../../.githooks/prepare-commit-msg");
const REFERENCE_TRANSACTION: &str = include_str!("../../.githooks/reference-transaction");
const POST_COMMIT: &str = include_str!("../../.githooks/post-commit");

/// Every hook this command installs, in (filename, embedded template) pairs.
const HOOKS: &[(&str, &str)] = &[
    ("pre-commit", PRE_COMMIT),
    ("pre-merge-commit", PRE_MERGE_COMMIT),
    ("pre-push", PRE_PUSH),
    ("prepare-commit-msg", PREPARE_COMMIT_MSG),
    ("reference-transaction", REFERENCE_TRANSACTION),
    ("post-commit", POST_COMMIT),
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
        GitCommand::Doctor(opts) => doctor_cmd(opts),
        GitCommand::Ship(opts) => ship_cmd(opts),
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

/// `true` on Unix when `path` has at least one executable bit set. Git
/// silently ignores a non-executable hook (just an advisory "hint", not an
/// error), so a content-correct-but-non-executable hook must NOT read as
/// installed -- confirmed live: this exact gap let a direct commit through
/// on `master` moments after this component's own commit landed, because
/// the merge hadn't yet brought in the executable-bit fix.
///
/// Always `true` on non-Unix: there's no POSIX exec bit to check, and
/// `install_hooks_for` never attempts to set one there either (matching git
/// for Windows' own model, where hook "executability" isn't a filesystem
/// permission).
#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|m| m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// `true` when `repo_root`'s hooks are already current: `core.hooksPath` is
/// `.githooks` and every file in `HOOKS` exists there, executable, with
/// content matching the embedded template. Used by both the CLI command (to
/// skip a no-op re-copy) and the `init`/`doctor` "githooks" component (to
/// report satisfied without touching the filesystem).
pub(crate) fn hooks_installed_for(repo_root: &Path) -> bool {
    let hooks_path =
        flare_git_core::shell::run_in_opt(repo_root, &["config", "--get", "core.hooksPath"]);
    if hooks_path.as_deref() != Some(".githooks") {
        return false;
    }
    HOOKS.iter().all(|(name, template)| {
        let dst = repo_root.join(".githooks").join(name);
        fs::read(&dst).ok().as_deref() == Some(template.as_bytes()) && is_executable(&dst)
    })
}

/// Writes the shared canonical templates (if missing), copies whichever of
/// `HOOKS` are missing or stale into `repo_root/.githooks/`, chmods +x
/// whichever aren't already executable (checked independently of content --
/// a content-correct file can still have lost its executable bit), and
/// points `core.hooksPath` at it if it isn't already. Returns whether
/// anything actually changed. Shared by the interactive CLI command and the
/// `init`/`doctor` "githooks" component -- same logic, same source of
/// truth, so the two can never drift apart on what "installed" means.
pub(crate) fn install_hooks_for(repo_root: &Path) -> Result<bool, String> {
    ensure_shared_templates().map_err(|e| format!("cannot write shared templates: {e}"))?;

    let local_dir = repo_root.join(".githooks");
    fs::create_dir_all(&local_dir).map_err(|e| format!("cannot create {local_dir:?}: {e}"))?;

    let mut changed = false;
    for (name, template) in HOOKS {
        let dst = local_dir.join(name);
        if fs::read(&dst).ok().as_deref() != Some(template.as_bytes()) {
            fs::write(&dst, template).map_err(|e| format!("writing {name}: {e}"))?;
            changed = true;
        }
        #[cfg(unix)]
        if !is_executable(&dst) {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dst, fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("chmod +x {name}: {e}"))?;
            changed = true;
        }
    }

    let current_hooks_path =
        flare_git_core::shell::run_in_opt(repo_root, &["config", "--get", "core.hooksPath"]);
    if current_hooks_path.as_deref() != Some(".githooks") {
        flare_git_core::shell::run_in(repo_root, &["config", "core.hooksPath", ".githooks"])
            .map_err(|e| format!("git config core.hooksPath: {e}"))?;
        changed = true;
    }

    Ok(changed)
}

fn install_hooks(opts: InstallHooksArgs) {
    let _ = opts;
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

    match install_hooks_for(&repo_root) {
        Ok(changed) => {
            for (name, _) in HOOKS {
                crate::ui::success(&format!(".githooks/{name}"));
            }
            crate::ui::success("core.hooksPath = .githooks");
            if changed {
                println!(
                    "\nBranch-protection hooks installed. Direct commits/pushes to the \
                     default branch are now blocked for every git client in this repo. \
                     Commits are also stamped with provenance trailers, every ref \
                     move is journaled to ~/.agentflare/audit/git-refs.jsonl, and \
                     lean-ctx's code index refreshes in the background after each commit."
                );
            }
        }
        Err(e) => crate::ui::error(&format!("agentflare git install-hooks: {e}")),
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
        } else if o.on_default_branch {
            " [on default branch]"
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

fn doctor_cmd(args: DoctorArgs) {
    let Some(repo_root) = resolve_repo_root("doctor") else {
        return;
    };
    let item_states = item_state_groups();
    let mut report = doctor::scan(&repo_root, args.staleness_days, &item_states);
    doctor::append_scope_check_violation(&mut report);
    if args.reclaim {
        let reclaimed = doctor::reclaim(&repo_root, &report, args.force);
        // Status lines go to stderr, not stdout -- `--format json` output on
        // stdout must stay machine-parseable (e.g. piped to `jq`).
        if reclaimed.is_empty() {
            eprintln!("No reclaimable lanes found.");
        } else {
            for name in &reclaimed {
                eprintln!("reclaimed: {}", name);
            }
        }
    }
    match args.format {
        DoctorFormat::Json => println!("{}", doctor::format_json(&report)),
        DoctorFormat::Markdown => println!("{}", doctor::format_markdown(&report)),
        DoctorFormat::Text => println!("{}", doctor::format_text(&report)),
    }
    if !report.violations.is_empty() {
        std::process::exit(1);
    }
}

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

/// Map of item `sequence_id` (as a string, matching `doctor::LaneHealth`) to
/// its state's `group_name` (e.g. "completed", "cancelled") — used by
/// `flare doctor` to flag a worktree as orphaned when the item behind it is
/// done but the worktree wasn't cleaned up. Best-effort: an empty map on any
/// DB error just means orphan detection silently finds nothing, matching
/// this file's existing soft-fail convention (see `claimed_sequence_ids`).
fn item_state_groups() -> HashMap<String, String> {
    let conn = match crate::db::open() {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    let mut stmt = match conn.prepare(
        "SELECT i.sequence_id, s.group_name FROM items i \
         JOIN states s ON i.state_id = s.id WHERE i.deleted_at IS NULL",
    ) {
        Ok(s) => s,
        Err(_) => return HashMap::new(),
    };
    match stmt.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?.to_string(), r.get::<_, String>(1)?))
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => HashMap::new(),
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

#[derive(serde::Serialize)]
struct ScopeCheckResult {
    deny: bool,
    reason: Option<String>,
    /// Set when scope-check itself could not classify the change (e.g. the
    /// pathset exceeds `MAX_CHANGED_PATHS`) -- a tooling limitation, not a
    /// deliberate policy verdict. Kept distinct from `deny`/`reason` so the
    /// shim can audit this as `ScopeCheckOutcome::Unavailable` /
    /// `Disposition::ScopeCheckError` rather than a real `Deny` (item #494).
    error: Option<String>,
}

fn scope_pass() -> ScopeCheckResult {
    ScopeCheckResult {
        deny: false,
        reason: None,
        error: None,
    }
}

fn scope_deny(reason: String) -> ScopeCheckResult {
    ScopeCheckResult {
        deny: true,
        reason: Some(reason),
        error: None,
    }
}

/// Like `scope_deny`, but for when scope-check itself couldn't reach a
/// verdict (a tooling limitation, e.g. the changed pathset exceeds
/// `MAX_CHANGED_PATHS`) rather than actually classifying the change. Still
/// blocks the operation (`deny: true`) since enforcement can't be skipped
/// just because classification failed, but sets `error` so the shim files
/// this under `Disposition::ScopeCheckError` instead of a real policy
/// `Deny` (item #494).
fn scope_error(reason: String) -> ScopeCheckResult {
    ScopeCheckResult {
        deny: true,
        reason: Some(reason.clone()),
        error: Some(reason),
    }
}

/// Splits this repo's live claims into "mine" (the invoker's own live claim,
/// if any) and "others" (every other live claim, scope-enforceable against).
///
/// Deliberately matches on the FULL `owner_id()` (`agent:instance`), not just
/// `claims::agent_of()` (agent type alone) -- claim ownership is documented
/// as instance-scoped (see `claims::owner_id`'s doc comment), and
/// `claim(action="release"|"done")` require an exact `owner_id()` match to
/// let go of a claim. Matching "mine" by agent-type alone would misclassify
/// a DIFFERENT session's live claim (a concurrent sibling, or one orphaned by
/// a crashed prior session) as the invoker's own -- producing a spurious
/// `OutOfTree` denial ("you hold claim X") for a claim the invoker doesn't
/// actually own and has no way to release.
fn partition_claims_by_owner(
    live: &[crate::claims::Claim],
    owner: &str,
) -> (Option<String>, Vec<scope::ClaimScope>) {
    let own_target = live
        .iter()
        .find(|c| c.owner == owner)
        .map(|c| c.target.clone());
    let others = live
        .iter()
        .filter(|c| c.owner != owner)
        .map(|c| scope::ClaimScope {
            target: c.target.clone(),
            owner: c.owner.clone(),
            scopes: c.scope.clone(),
        })
        .collect();
    (own_target, others)
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
    let (own_target, others) = partition_claims_by_owner(&live, &owner);

    let in_worktree = branch::is_linked_worktree(&repo_root);
    // Only skip computing changed paths when nothing downstream depends on
    // them: with no enforceable other-claim scope, nothing can overlap. An
    // out-of-tree invoker must still get a *non-empty* pathset here --
    // `classify_scopes` only reaches its `OutOfTree` check past its
    // `changed_paths.is_empty() -> Clear` short-circuit, so skipping the
    // diff for `out_of_tree` would silently turn a real OutOfTree denial
    // into a pass. This is also the memory fix for the scope-check
    // child-process OOM (item #472): `git diff <default>...<head>
    // --name-only` on a branch that diverged hugely from the default branch
    // lists the whole tree as a multi-MB pathset, which previously crashed
    // the child on a memory-constrained environment and surfaced to the
    // shim as a fake policy denial.
    let out_of_tree = own_target.is_some() && !in_worktree;
    let has_enforced_others = others
        .iter()
        .any(|c| !scope::scope_is_wildcard_or_empty(&c.scopes));
    let changed = if !out_of_tree && !has_enforced_others {
        Vec::new()
    } else {
        match changed_paths(&repo_root, subcommand) {
            Ok(p) => p,
            Err(e) => {
                return scope_error(format!("scope-check could not classify changed paths: {e}"));
            }
        }
    };
    match scope::classify_scopes(&changed, own_target.as_deref(), in_worktree, &others) {
        scope::ScopeVerdict::Overlapping {
            owner,
            target,
            scope,
        } => scope_deny(format!(
            "this touches path(s) inside claim '{target}' (owner {owner}, scope '{scope}') -- work inside that claim's own worktree, or coordinate with {owner}."
        )),
        scope::ScopeVerdict::OutOfTree { target } => scope_deny(format!(
            "you hold claim '{target}' -- do this work in its isolated worktree, not the canonical checkout. If '{target}' is an item, call `item(action=\"claim\", id=<item>)` for that same item to get its worktree path (this claim doesn't have one yet if it was acquired through the standalone `claim`/`mcp__flare__claim` tool, which only takes a scope lock -- `git worktree add` itself is blocked, so that command alone won't get you there)."
        )),
        scope::ScopeVerdict::Clear | scope::ScopeVerdict::Related => scope_pass(),
    }
}

/// Cap on the number of changed paths a commit/push scope-check will
/// classify. Beyond this the pathset is too large to classify reliably, and
/// streaming it unbounded is exactly what OOM'd the scope-check child in the
/// first place (item #472) -- so the check fails closed with a clear message
/// instead of crashing the child process.
const MAX_CHANGED_PATHS: usize = 50_000;

fn too_many_paths_msg(cap: usize) -> String {
    format!(
        "the changed pathset exceeds the {cap}-path scope-check limit and cannot be classified; shrink this commit/push and retry"
    )
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
///
/// `Err` is reserved for the one case where fail-open would be wrong: the
/// changed pathset is so large it cannot be classified at all (see
/// `MAX_CHANGED_PATHS`). The diff is streamed with a line cap so a
/// pathological pathset can't blow the child process's memory first.
fn changed_paths(repo_root: &Path, subcommand: &str) -> Result<Vec<String>, String> {
    let cap = MAX_CHANGED_PATHS;
    if subcommand == "commit" {
        let mut paths =
            match shell::run_in_lines_bounded(repo_root, &["diff", "--cached", "--name-only"], cap)
            {
                Ok(p) => p,
                Err(BoundedLinesError::Git(_)) => Vec::new(), // fail-open on diff resolution
                Err(BoundedLinesError::TooManyLines) => return Err(too_many_paths_msg(cap)),
            };
        match shell::run_in_lines_bounded(repo_root, &["diff", "--name-only"], cap) {
            Ok(p) => paths.extend(p),
            Err(BoundedLinesError::Git(_)) => {}
            Err(BoundedLinesError::TooManyLines) => return Err(too_many_paths_msg(cap)),
        }
        paths.sort();
        paths.dedup();
        // Each diff above is capped individually, so a disjoint staged set
        // and working-tree set can each land at `cap` and union past it --
        // enforce the documented `MAX_CHANGED_PATHS` limit on the merged,
        // deduplicated set too.
        if paths.len() > cap {
            return Err(too_many_paths_msg(cap));
        }
        return Ok(paths);
    }
    let range_args: Vec<String> = match subcommand {
        "push" => {
            let default_branch = branch::resolve_default_branch(repo_root);
            let current =
                branch::current_branch(repo_root).unwrap_or_else(|| default_branch.clone());
            let range = format!("{default_branch}...{current}");
            vec!["diff".to_string(), "--name-only".to_string(), range]
        }
        _ => return Ok(Vec::new()),
    };
    let args: Vec<&str> = range_args.iter().map(String::as_str).collect();
    match shell::run_in_lines_bounded(repo_root, &args, cap) {
        Ok(p) => Ok(p),
        Err(BoundedLinesError::Git(_)) => Ok(Vec::new()), // fail-open on diff resolution
        Err(BoundedLinesError::TooManyLines) => Err(too_many_paths_msg(cap)),
    }
}

/// The "ship it" macro: push the current branch, open (or reuse) its PR,
/// then poll CI -- collapsing what's otherwise 3-4 separate `git`/`gh`
/// calls (or `flare_git` MCP actions `pr_create`+`pr_wait`) into one.
/// Deliberately never stages or commits: matches `item done`'s existing
/// push_and_open_pr convention of only ever pushing what's already
/// committed, so ship can't silently sweep up unrelated dirty files.
fn ship_cmd(opts: ShipArgs) {
    use crate::github::{Client, RepoId, pulls};

    let Some(repo_root) = resolve_repo_root("ship") else {
        return;
    };

    match shell::run_in(&repo_root, &["status", "--porcelain"]) {
        Ok(s) if !s.is_empty() => {
            crate::ui::error("agentflare git ship: uncommitted changes -- commit them first");
            std::process::exit(1);
        }
        Err(e) => {
            crate::ui::error(&format!("agentflare git ship: {e}"));
            std::process::exit(1);
        }
        _ => {}
    }

    let Some(head) = branch::current_branch(&repo_root) else {
        crate::ui::error("agentflare git ship: could not resolve the current branch");
        std::process::exit(1);
    };
    let base = opts
        .base
        .clone()
        .unwrap_or_else(|| branch::resolve_default_branch(&repo_root));
    if head == base {
        crate::ui::error(&format!(
            "agentflare git ship: on '{base}' itself -- checkout a feature branch first"
        ));
        std::process::exit(1);
    }

    if shell::run_in(&repo_root, &["rev-parse", "--verify", "--quiet", &base]).is_err() {
        crate::ui::error(&format!(
            "agentflare git ship: base '{base}' does not resolve in this repo -- pass --base explicitly"
        ));
        std::process::exit(1);
    }
    let ahead = match shell::run_in(
        &repo_root,
        &["rev-list", "--count", &format!("{base}..{head}")],
    ) {
        Ok(n) => n,
        Err(e) => {
            crate::ui::error(&format!(
                "agentflare git ship: could not count commits: {e}"
            ));
            std::process::exit(1);
        }
    };
    if ahead == "0" {
        crate::ui::error(&format!(
            "agentflare git ship: '{head}' has no commits ahead of '{base}' -- nothing to ship"
        ));
        std::process::exit(1);
    }

    // Resolved and validated before the push so an invalid title fails
    // before anything remote happens -- every failure past this point only
    // warns and returns instead of a hard exit, since the push has already
    // landed by then and there's nothing left to abort.
    let title = opts
        .title
        .clone()
        .unwrap_or_else(|| default_pr_title(&repo_root, &head));
    if let Err(e) = crate::mcp_server::AgentflareMcp::validate_conventional_pr_title(&title) {
        crate::ui::error(&format!(
            "agentflare git ship: {e} -- pass --title explicitly"
        ));
        std::process::exit(1);
    }

    crate::ui::step(&format!("pushing {head}..."));
    if let Err(e) = shell::run_in(&repo_root, &["push", "-u", "origin", &head]) {
        crate::ui::error(&format!("agentflare git ship: push failed: {e}"));
        std::process::exit(1);
    }

    let Some(repo) = RepoId::resolve_from_remote(&repo_root) else {
        crate::ui::warning("pushed, but could not resolve the origin remote -- open a PR manually");
        return;
    };
    let client = match Client::new() {
        Ok(c) => c,
        Err(e) => {
            crate::ui::warning(&format!(
                "pushed, but no GitHub credentials ({e}) -- open a PR manually"
            ));
            return;
        }
    };

    // `find_existing` matches open/closed/merged PRs alike (by design, for
    // callers like `item done` that want to avoid re-opening a duplicate
    // after a merge). `ship` needs the opposite for a closed/merged match:
    // the caller just pushed new commits ahead of `base`, so that's new
    // work needing a fresh PR, not a stale closed one to report as "open".
    let pr = match pulls::find_existing(&client, &repo, &head) {
        Ok(Some(existing)) if existing.state == "open" => {
            crate::ui::success(&format!("PR already open: {}", existing.html_url));
            existing
        }
        Ok(_) => {
            let body = opts
                .body
                .clone()
                .unwrap_or_else(|| default_pr_body(&repo_root, &base, &head));
            match pulls::create(&client, &repo, &title, &head, &base, Some(&body)) {
                Ok(pr) => {
                    crate::ui::success(&format!("opened PR #{}: {}", pr.number, pr.html_url));
                    pr
                }
                Err(e) => {
                    crate::ui::warning(&format!("pushed, but PR creation failed: {e}"));
                    return;
                }
            }
        }
        Err(e) => {
            crate::ui::warning(&format!(
                "pushed, but could not check for an existing PR: {e}"
            ));
            return;
        }
    };

    remember_shipped(&repo_root, &repo, &base, &head, &pr);

    if opts.no_wait {
        return;
    }
    wait_for_checks(&client, &repo, &pr, opts.wait_secs);
}

/// Records that a PR was shipped in agentflare's own memory store, so a
/// later session (this one or another agent's) can `memory recall` it
/// instead of re-discovering the work from scratch. Best-effort: a memory
/// write must never fail a ship that already succeeded, so a `remember`
/// error only prints a warning.
fn remember_shipped(
    repo_root: &Path,
    repo: &crate::github::RepoId,
    base: &str,
    head: &str,
    pr: &crate::github::models::PullRequest,
) {
    let log = default_pr_body(repo_root, base, head);
    let content = format!("{log}\n\n{}", pr.html_url);
    let input = crate::memory::mcp::RememberInput {
        title: format!("Shipped: {}", pr.title),
        content,
        r#type: "decision".to_string(),
        session_id: None,
        project: Some(repo.repo.clone()),
        topic_key: Some(format!("pr-{}", pr.number)),
        scope: None,
    };
    if let Err(e) = crate::memory::mcp::handle_remember(input) {
        crate::ui::warning(&format!("PR shipped, but memory remember failed: {e}"));
    }
}

/// Falls back to the branch's latest commit subject when `--title` is
/// omitted -- the same "just use the commit message" default a human
/// running `gh pr create --fill` gets.
fn default_pr_title(repo_root: &Path, branch_name: &str) -> String {
    shell::run_in(repo_root, &["log", "-1", "--format=%s", branch_name])
        .unwrap_or_else(|_| format!("ship {branch_name}"))
}

/// Falls back to a bullet list of `base..head` commit subjects when
/// `--body` is omitted.
fn default_pr_body(repo_root: &Path, base: &str, head: &str) -> String {
    let log = shell::run_in(
        repo_root,
        &["log", "--format=- %s", &format!("{base}..{head}")],
    )
    .unwrap_or_default();
    if log.is_empty() {
        format!("Shipped via `agentflare git ship` ({base}..{head}).")
    } else {
        log
    }
}

/// Bounded synchronous poll -- v1 scope deliberately stops here rather than
/// a TUI-style live-refresh loop (see item #292's non-goal). Reuses
/// `github::mcp::checks_wait_summary` so the pending/failed classification
/// stays identical to the `flare_git` MCP tool's `pr_wait` action.
fn wait_for_checks(
    client: &crate::github::Client,
    repo: &crate::github::RepoId,
    pr: &crate::github::models::PullRequest,
    wait_secs: u64,
) {
    let Some(sha) = pr.head.as_ref().map(|h| h.sha.clone()) else {
        return;
    };
    crate::ui::step("waiting for CI checks...");
    let start = std::time::Instant::now();
    loop {
        let checks = match crate::github::actions::list_check_runs(client, repo, &sha) {
            Ok(c) => c,
            Err(e) => {
                crate::ui::warning(&format!("could not fetch check status: {e}"));
                return;
            }
        };
        let summary = crate::github::mcp::checks_wait_summary(&checks, start.elapsed().as_secs());
        let total = summary["total_checks"].as_u64().unwrap_or(0);
        // Zero checks means no workflow has registered against the head SHA
        // yet (common in the first few seconds after a push) -- not a green
        // build, so keep polling instead of reporting a false success.
        if total > 0 && !summary["pending"].as_bool().unwrap_or(false) {
            let failed: Vec<String> = summary["failed_checks"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if failed.is_empty() {
                crate::ui::success(&format!("CI green ({total} checks)"));
            } else {
                crate::ui::error(&format!("CI failed: {}", failed.join(", ")));
            }
            return;
        }
        if start.elapsed().as_secs() >= wait_secs {
            crate::ui::warning(&format!(
                "still pending after {wait_secs}s -- check manually: {}",
                pr.html_url
            ));
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(10));
    }
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

    fn claim_fixture(target: &str, owner: &str, scope: Vec<String>) -> crate::claims::Claim {
        crate::claims::Claim {
            repo: "o/r".to_string(),
            target: target.to_string(),
            owner: owner.to_string(),
            status: "claimed".to_string(),
            created_at: 0,
            heartbeat_at: 0,
            git_commit: None,
            scope,
            stale: false,
        }
    }

    #[test]
    fn worktree_teardown_deny_names_a_parsable_cli() {
        // Vent #469: the shim's teardown deny message used to suggest
        // `agentflare git worktree audit --prune`, which is not a command
        // this CLI has -- an agent following it verbatim got a clap parse
        // error. Parse the suggestion against the real command tree rather
        // than string-comparing it, so a later rename of `audit`/`prune`
        // fails here instead of silently rotting the deny message again.
        use clap::Parser as _;
        let suggested = classify::WORKTREE_PRUNE_COMMAND;
        let cli = crate::cli::Cli::try_parse_from(suggested.split_whitespace()).unwrap_or_else(
            |e| panic!("the worktree-teardown deny message suggests `{suggested}`, which does not parse: {e}"),
        );
        let Some(crate::cli::Commands::Git(GitArgs {
            command:
                GitCommand::Audit(WorktreeAuditArgs {
                    command: WorktreeAuditCommand::Prune(prune),
                }),
        })) = cli.command
        else {
            panic!("`{suggested}` must resolve to `git audit prune`");
        };
        assert!(
            prune.all,
            "the suggestion must prune every orphan -- a denied agent has no \
             list of names to pass"
        );
    }

    #[test]
    fn partition_claims_by_owner_does_not_treat_a_different_instance_as_mine() {
        // item #444: a claim from a DIFFERENT session of the same agent type
        // (e.g. orphaned by a crashed prior session) must never be
        // classified as "own_target" -- that produces a spurious OutOfTree
        // denial for a claim the invoker cannot release (release/done are
        // exact-owner-scoped).
        let live = vec![claim_fixture("item#1", "claude-code:28604", vec![])];
        let (own_target, others) = partition_claims_by_owner(&live, "claude-code:14432");
        assert_eq!(own_target, None);
        assert_eq!(others.len(), 1);
        assert_eq!(others[0].owner, "claude-code:28604");
    }

    #[test]
    fn partition_claims_by_owner_matches_the_exact_same_instance() {
        let live = vec![claim_fixture("item#1", "claude-code:14432", vec![])];
        let (own_target, others) = partition_claims_by_owner(&live, "claude-code:14432");
        assert_eq!(own_target, Some("item#1".to_string()));
        assert!(others.is_empty());
    }

    #[test]
    fn partition_claims_by_owner_enforces_scope_between_sibling_instances() {
        // Second-order effect of the same bug: two concurrent sessions of
        // the same agent type got zero scope enforcement against each other
        // because agent-type equality swept both into neither list
        // correctly. A sibling instance's scoped claim must land in
        // `others`, not be silently dropped.
        let live = vec![claim_fixture(
            "item#2",
            "claude-code:99999",
            vec!["crates/foo/".to_string()],
        )];
        let (own_target, others) = partition_claims_by_owner(&live, "claude-code:14432");
        assert_eq!(own_target, None);
        assert_eq!(others.len(), 1);
        assert_eq!(others[0].scopes, vec!["crates/foo/".to_string()]);

        // The regression this test is actually about: that placement in
        // `others` translates into real enforcement -- a changed path inside
        // the sibling's declared scope must classify as Overlapping.
        let verdict = scope::classify_scopes(
            &["crates/foo/src/lib.rs".to_string()],
            own_target.as_deref(),
            false,
            &others,
        );
        assert_eq!(
            verdict,
            scope::ScopeVerdict::Overlapping {
                owner: "claude-code:99999".to_string(),
                target: "item#2".to_string(),
                scope: "crates/foo/".to_string(),
            }
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
    fn install_hooks_for_writes_all_hooks_and_sets_core_hooks_path() {
        let repo = init_repo();
        let changed = install_hooks_for(repo.path()).unwrap();
        assert!(changed);
        for (name, template) in HOOKS {
            let content = std::fs::read(repo.path().join(".githooks").join(name)).unwrap();
            assert_eq!(
                content,
                template.as_bytes(),
                "{name} should match the embedded template"
            );
        }
        let hooks_path =
            flare_git_core::shell::run_in_opt(repo.path(), &["config", "--get", "core.hooksPath"]);
        assert_eq!(hooks_path.as_deref(), Some(".githooks"));
    }

    #[test]
    fn hooks_installed_for_reflects_install_state() {
        let repo = init_repo();
        assert!(!hooks_installed_for(repo.path()), "nothing installed yet");
        install_hooks_for(repo.path()).unwrap();
        assert!(
            hooks_installed_for(repo.path()),
            "should report installed after install_hooks_for"
        );
    }

    #[test]
    fn install_hooks_for_is_idempotent() {
        let repo = init_repo();
        assert!(
            install_hooks_for(repo.path()).unwrap(),
            "first install changes something"
        );
        assert!(
            !install_hooks_for(repo.path()).unwrap(),
            "second install on an already-current repo must report no change"
        );
    }

    #[test]
    fn install_hooks_for_repairs_a_stale_hand_edited_hook() {
        let repo = init_repo();
        install_hooks_for(repo.path()).unwrap();
        std::fs::write(
            repo.path().join(".githooks").join("pre-commit"),
            "tampered\n",
        )
        .unwrap();

        assert!(
            !hooks_installed_for(repo.path()),
            "tampered hook must not read as installed"
        );
        let changed = install_hooks_for(repo.path()).unwrap();
        assert!(changed, "a stale hook must be rewritten");
        let content = std::fs::read(repo.path().join(".githooks").join("pre-commit")).unwrap();
        assert_eq!(content, PRE_COMMIT.as_bytes());
    }

    #[test]
    #[cfg(unix)]
    fn install_hooks_for_repairs_a_hook_that_lost_its_executable_bit() {
        // Content-correct but not executable: git silently ignores the hook
        // (an advisory hint, not an error) rather than running it -- so a
        // check that only compares content would report "installed" on a
        // hook that in practice never fires. Confirmed live: this exact gap
        // let a direct commit through on master moments after this
        // component's own fix commit landed, because the merge hadn't yet
        // brought the executable-bit fix into the working tree.
        use std::os::unix::fs::PermissionsExt;
        let repo = init_repo();
        install_hooks_for(repo.path()).unwrap();
        let dst = repo.path().join(".githooks").join("pre-commit");
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o644)).unwrap();

        assert!(
            !hooks_installed_for(repo.path()),
            "a non-executable hook must not read as installed, even with correct content"
        );
        let changed = install_hooks_for(repo.path()).unwrap();
        assert!(changed, "the lost executable bit must be restored");
        assert!(is_executable(&dst));
        // Content untouched -- only the mode needed fixing.
        assert_eq!(std::fs::read(&dst).unwrap(), PRE_COMMIT.as_bytes());
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
        let paths = changed_paths(repo.path(), "commit").unwrap();
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
        let paths = changed_paths(repo.path(), "commit").unwrap();
        assert_eq!(
            paths.iter().filter(|p| *p == "new.txt").count(),
            1,
            "{paths:?}"
        );
    }

    #[test]
    fn changed_paths_for_commit_is_empty_when_clean() {
        let repo = init_repo();
        assert!(changed_paths(repo.path(), "commit").unwrap().is_empty());
    }

    /// Item #494: `scope_error` (used when `changed_paths()` can't classify
    /// the change, e.g. cap-exceeded) must still block via `deny: true`, but
    /// carry `error` too -- distinct from `scope_deny`'s real policy
    /// verdict -- so the shim can tell the two apart on the wire.
    #[test]
    fn scope_error_sets_both_deny_and_error() {
        let result = scope_error("too many paths".to_string());
        assert!(result.deny);
        assert_eq!(result.reason.as_deref(), Some("too many paths"));
        assert_eq!(result.error.as_deref(), Some("too many paths"));

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains(r#""error":"too many paths""#), "{json}");
    }

    #[test]
    fn scope_deny_leaves_error_unset() {
        let result = scope_deny("overlapping claim".to_string());
        assert!(result.deny);
        assert_eq!(result.error, None);
    }
}
