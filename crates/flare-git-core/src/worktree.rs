//! Worktree lifecycle management: isolates work items into per-branch git
//! worktrees, resolves target branches from parent item metadata, and keeps
//! each worktree's Cargo target dir isolated.
//!
//! GitHub-API concerns (opening a PR once a branch is pushed) are
//! deliberately NOT here — `push_branch` below only handles the local push
//! mechanics and returns the pushed branch name; opening the PR is the
//! caller's job (see the thin wrapper in the main binary's
//! `src/worktree.rs`), so this crate stays free of any GitHub dependency.

use std::path::{Path, PathBuf};

use agentflare_backend::item::Item;

use crate::branch::resolve_default_branch;
use crate::shell::{run_in as run_git_in, run_in_ok as run_git_in_ok};

#[path = "worktree_heal.rs"]
mod heal;
#[path = "worktree_orphans.rs"]
mod orphans;
#[path = "worktree_process.rs"]
mod process;

use heal::*;
use orphans::remove_worktree_dir;
pub use orphans::{OrphanWorktree, audit_orphans, gc_orphans};
pub(crate) use process::run_output_timeout;
use process::*;

/// Minimal progress-reporting interface — decouples this crate from the
/// main binary's MCP-specific `ProgressSender` (which depends on `rmcp`),
/// so this leaf crate has no reason to know anything about MCP.
pub trait Progress {
    fn send(&self, progress: f64, total: Option<f64>, message: Option<String>);
}

/// Serializes `git worktree add` invocations against `.git/config`/
/// `.git/worktrees` admin state -- see the comment at its use site in
/// `create_worktree` below.
static WORKTREE_ADD_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// True when a `git worktree add` failure looks transient — a registration/
/// lock race with teardown/cleanup still in flight — rather than structural.
///
/// Matched case-insensitively against the subprocess error text. Callers use
/// this to retry the same job with a delay (intra-job `retry_after_secs`)
/// instead of failing terminal and churning through rapid-fire redispatches
/// of fresh jobs that all hit the same half-torn-down admin state (item #633).
#[must_use]
pub fn is_retryable_worktree_race(err: &str) -> bool {
    let e = err.to_lowercase();
    e.contains("missing but already registered")
        || e.contains("missing but locked")
        || e.contains("already registered")
        || e.contains("could not lock")
        || e.contains("unable to lock")
        || e.contains("already locked")
        || e.contains("cannot lock ref")
        || (e.contains("unable to create") && e.contains(".lock"))
        || (e.contains("worktree add") && e.contains("already exists"))
}

/// `.worktrees/task/<sequence_id>` under `repo_root` -- the one place every
/// per-item worktree lives.
#[must_use]
pub fn item_worktree_path(repo_root: &Path, sequence_id: i64) -> PathBuf {
    repo_root
        .join(".worktrees")
        .join("task")
        .join(sequence_id.to_string())
}

/// Deadline for one `git worktree add`, checkout hooks included.
const WORKTREE_ADD_TIMEOUT_SECS: u64 = 600;

pub fn resolve_target_branch(conn: &rusqlite::Connection, item: &Item, repo_root: &Path) -> String {
    // A finished parent's branch has landed (or been abandoned): basing new
    // work on it would carry its pre-squash commits into this item's PR and
    // conflict on every later rebase onto the default branch.
    if let Some(ref parent_id) = item.parent_id
        && let Ok(parent) = agentflare_backend::item::get(conn, parent_id)
        && !agentflare_backend::state::get(conn, &parent.state_id)
            .is_ok_and(|st| matches!(st.group_name.as_str(), "completed" | "cancelled"))
        && let Ok(meta) = serde_json::from_str::<serde_json::Value>(&parent.metadata)
        && let Some(branch) = meta.get("branch").and_then(|v| v.as_str())
    {
        return branch.to_string();
    }
    resolve_default_branch(repo_root)
}

#[must_use]
pub fn already_isolated_for(branch: &str, repo_root: &Path) -> bool {
    let git_dir = match run_git_in(repo_root, &["rev-parse", "--git-dir"]) {
        Ok(d) => d,
        Err(_) => return false,
    };
    let common_dir = match run_git_in(repo_root, &["rev-parse", "--git-common-dir"]) {
        Ok(d) => d,
        Err(_) => return false,
    };
    if git_dir == common_dir {
        return false;
    }
    // Exits 0 with EMPTY stdout in a plain linked worktree (not a git
    // submodule) — only a non-empty path means we're actually inside a
    // submodule's own superproject relationship, which is the case this
    // guard exists to rule out.
    if let Ok(out) = run_git_in(
        repo_root,
        &["rev-parse", "--show-superproject-working-tree"],
    ) && !out.is_empty()
    {
        return false;
    }
    match run_git_in(repo_root, &["branch", "--show-current"]) {
        Ok(b) => b == branch,
        Err(_) => false,
    }
}

/// `true` if `worktree_path` already exists on disk and is itself a git
/// worktree checked out to `branch` -- the on-disk counterpart to
/// `already_isolated_for` above, for callers running from outside the
/// worktree (the daemon's normal case) rather than from inside it.
#[must_use]
fn worktree_already_checked_out(worktree_path: &Path, branch: &str) -> bool {
    if !worktree_path.is_dir() {
        return false;
    }
    match run_git_in(worktree_path, &["branch", "--show-current"]) {
        Ok(b) => b == branch,
        Err(_) => false,
    }
}

/// Adds `.worktrees/` and `.cargo/` to this repo's LOCAL, untracked ignore
/// rules (`.git/info/exclude`) rather than the tracked `.gitignore` — a
/// claim should never create a commit in the caller's repository (would
/// sweep up any unrelated staged files, and any pre-existing uncommitted
/// `.gitignore` edits, into a commit the agent didn't ask for).
///
/// `.cargo/` joined `.worktrees/` here because `isolate_worktree_target_dir`
/// writes an untracked `.cargo/config.toml` into every worktree it creates;
/// left unignored, `git status --porcelain` reports every freshly-created
/// worktree as dirty before any real work happens in it, which is exactly
/// the signal `cleanup_item_worktree` uses to decide it's unsafe to remove
/// (item #420).
pub fn ensure_worktrees_ignored(repo_root: &Path) {
    let Ok(common_dir) = run_git_in(repo_root, &["rev-parse", "--git-common-dir"]) else {
        return;
    };
    let exclude_path = repo_root.join(common_dir).join("info").join("exclude");
    let mut content = std::fs::read_to_string(&exclude_path).unwrap_or_default();
    let mut changed = false;
    for pat in [".worktrees/", ".cargo/"] {
        if content.lines().any(|l| l.trim() == pat) {
            continue;
        }
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(pat);
        content.push('\n');
        changed = true;
    }
    if !changed {
        return;
    }
    if let Some(parent) = exclude_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if std::fs::write(&exclude_path, content).is_err() {
        eprintln!("worktree: failed to write .git/info/exclude");
    }
}

/// Warns (does not fail) when an ambient `CARGO_TARGET_DIR` is set in the
/// environment at claim time. A shared `CARGO_TARGET_DIR` across worktrees
/// is a silent correctness bug: Cargo's fingerprint hash omits the worktree
/// path, so two worktrees of the same repo reuse each other's stale local
/// crate artifacts (cargo #12516/#14053/#7740; OpenBlob #522).
///
/// This function is the last-resort warning, not the fix — it only covers a
/// developer who opens a bare shell inside a worktree and runs `cargo`
/// directly, bypassing `agentflare run`. Per Cargo's precedence (CLI flag >
/// env var > config file), an ambient `CARGO_TARGET_DIR` *always* wins over
/// the `.cargo/config.toml` that `isolate_worktree_target_dir` writes, so in
/// that bypass case the isolated `target/` is silently shadowed and the bug
/// can still occur.
///
/// Item #139 closed the two paths that matter for agents: `run_launch_env`/
/// `run_headless` (src/agent_launch.rs) strip `CARGO_TARGET_DIR` from every
/// launched agent's child env — the only mechanism that actually outranks
/// the var — and CI (`.github/workflows/ci.yml`'s `target-dir-guard` job)
/// fails the build outright if the var is set project-wide.
fn warn_if_ambient_target_dir() {
    if std::env::var_os("CARGO_TARGET_DIR").is_some() {
        eprintln!(
            "worktree: ambient CARGO_TARGET_DIR is set — it is SHARED across worktrees and \
             can leak stale artifacts between divergent checkouts. Prefer trusting CI for \
             local test builds, or unset it and rely on the worktree's isolated target dir."
        );
    }
}

/// Writes a per-worktree `.cargo/config.toml` so the worktree's `target/`
/// resolves locally instead of inheriting a shared `CARGO_TARGET_DIR`.
///
/// Caveat: this only takes effect when `CARGO_TARGET_DIR` is *unset* in the
/// ambient environment — no config file can outrank the env var (Cargo's
/// precedence is CLI flag > env var > config file). A bare shell that
/// bypasses `agentflare run` still needs `warn_if_ambient_target_dir`'s
/// warning; every agent-launched build IS covered, since item #139 made
/// `run_launch_env`/`run_headless` (src/agent_launch.rs) strip the var from
/// the child env before it ever reaches Cargo, and CI enforces the same
/// invariant via the `target-dir-guard` job in ci.yml.
///
/// Local workspace crates must NOT be shared across worktrees (silent
/// contamination); registry deps are safe but are better served by a shared
/// sccache. A relative `target-dir = "target"` resolves per-checkout, giving
/// each worktree its own isolated cache. When `sccache` is on `PATH`, also
/// wires it up as the `rustc-wrapper` with `SCCACHE_BASEDIRS` set to this
/// worktree's own absolute path — sccache hashes absolute source paths into
/// its cache key by default, so without stripping that prefix, identical
/// dependency source in a sibling worktree would never hit
/// (mozilla/sccache#196; a `--remap-path-prefix` rustflag looks tempting but
/// itself varies per worktree and defeats the cache key instead). Soft-fails
/// (eprintln) — never blocks a claim.
fn isolate_worktree_target_dir(worktree_path: &Path) {
    let cargo_dir = worktree_path.join(".cargo");
    let _ = std::fs::create_dir_all(&cargo_dir);
    let config_path = cargo_dir.join("config.toml");
    if config_path.exists() {
        return; // don't clobber an intentional worktree-local override
    }
    let mut content = "[build]\n# Isolated per worktree (see item #133). Registry deps are\n\
                   # better shared via sccache (RUSTC_WRAPPER + SCCACHE_BASEDIRS),\n\
                   # not a shared CARGO_TARGET_DIR, which leaks artifacts across worktrees.\n\
                   target-dir = \"target\"\n"
        .to_string();
    if sccache_available() {
        // TOML literal strings ('...') can't escape a single quote, so a
        // worktree path containing one (e.g. "C:\Users\John's PC\repo")
        // would produce invalid TOML. Use a basic string instead, with
        // backslashes and double quotes escaped.
        let escaped_path = worktree_path
            .to_string_lossy()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        content.push_str(&format!(
            "rustc-wrapper = \"sccache\"\n\n[env]\nSCCACHE_BASEDIRS = \"{escaped_path}\"\n"
        ));
    }
    if let Err(e) = std::fs::write(&config_path, content) {
        eprintln!(
            "worktree: could not write isolated .cargo/config.toml for {}: {e}",
            worktree_path.display()
        );
    }
}

/// True when the `sccache` binary is reachable on `PATH`.
fn sccache_available() -> bool {
    flare_process::command("sccache")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Max length of the slug portion of a `task/<sequence_id>-<slug>` branch
/// name (excludes the `task/<sequence_id>-` prefix).
const MAX_SLUG_LEN: usize = 40;

/// Lowercase, hyphen-collapsed form of `item.name` for use in a branch
/// name segment. Non-ASCII-alphanumeric runs (including unicode) collapse
/// to a single `-`; leading/trailing `-` are trimmed; result is truncated
/// to `MAX_SLUG_LEN`. Empty for an empty or all-symbol/unicode name.
fn slugify_branch_title(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out.truncate(MAX_SLUG_LEN);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Deterministic `task/<sequence_id>[-<slug>]` branch name for `item` --
/// same sequence_id + same name always yields the same string, which
/// `already_isolated_for` (above) relies on for its exact-match re-claim
/// detection. Falls back to the bare `task/<sequence_id>` (no trailing
/// dash) when `item.name` has nothing sluggable in it.
///
/// `pub`, not `pub(crate)`: `src/worktree.rs` (a different crate) also
/// needs to resolve an item's branch name for `branch_diverged`,
/// `is_pr_merged`, and `pr_ci_status` -- those independently recomputing
/// the bare `format!("task/{}", item.sequence_id)` form is exactly the
/// bug this function exists to prevent (item #89).
pub fn task_branch_name(item: &Item) -> String {
    let slug = slugify_branch_title(&item.name);
    if slug.is_empty() {
        format!("task/{}", item.sequence_id)
    } else {
        format!("task/{}-{slug}", item.sequence_id)
    }
}

/// The branch `item.metadata.pr.branch` already names -- set by
/// `discover_untracked_prs` for a PR opened by hand (outside the item-done
/// flow) and by `persist_pr_identity` for one opened through it, both in the
/// same `{"pr":{"number":N,"branch":"..."}}` shape. When present, this is
/// the actual branch the item's PR lives on and must win over
/// `task_branch_name`'s sequence-id-derived guess -- for a hand-opened PR
/// that guess names a branch that has nothing to do with the PR at all,
/// which is exactly how dispatching self-repair on a `discover_untracked_prs`
/// item created a brand-new orphan branch and a duplicate PR instead of
/// continuing the existing one (confirmed live against image-qc item #19 /
/// PR #311: self-repair pushed `task/19-...` and opened duplicate PR #314).
fn tracked_pr_branch(item: &Item) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(&item.metadata)
        .ok()?
        .get("pr")?
        .get("branch")?
        .as_str()
        .map(str::to_string)
}

/// The branch `item`'s worktree is (or should be) on: whatever a worktree
/// already sitting at `worktree_path` is *actually* checked out to, falling
/// back to `tracked_pr_branch` (the item's own known PR branch) or, absent
/// that, the freshly-derived `task_branch_name`.
///
/// `task_branch_name` recomputes its slug from `item.name` every call, so it
/// silently changes if the item gets renamed between claims -- and a
/// worktree created before this slugged naming scheme existed simply sits on
/// the bare `task/<sequence_id>`. Either way, re-deriving a slug that
/// doesn't match what's already checked out makes `create_worktree`'s
/// re-claim detection miss the existing worktree, so `git worktree add`
/// collides with the already-occupied path instead of reusing it (item
/// #459's dispatch-blocking loop on item #447, which predates the slugged
/// scheme). The on-disk check also accepts `tracked_pr_branch`'s value as a
/// valid current checkout, not just the `task/<seq>[-slug]` shapes -- without
/// that, a second dispatch onto an item already correctly sitting on its
/// tracked PR branch would fail this match and re-derive `task_branch_name`
/// all over again, undoing the fix on every dispatch after the first.
fn resolve_worktree_branch(item: &Item, worktree_path: &Path) -> String {
    let expected = tracked_pr_branch(item).unwrap_or_else(|| task_branch_name(item));
    if worktree_path.is_dir()
        && let Ok(current) = run_git_in(worktree_path, &["branch", "--show-current"])
        && !current.is_empty()
        && (current == expected
            || current == format!("task/{}", item.sequence_id)
            || current.starts_with(&format!("task/{}-", item.sequence_id)))
    {
        return current;
    }
    expected
}

/// The branch `item`'s worktree is (or should be) on — whatever is
/// actually checked out at `.worktrees/task/<sequence_id>`, falling back
/// to [`task_branch_name`] only when no worktree exists yet. `push_branch`
/// and `item_done`'s divergence classification must both use this instead
/// of recomputing `task_branch_name` alone: a rename (or a worktree
/// created before the slugged naming scheme) leaves the checkout on the
/// old `task/<N>` or `task/<N>-<old-slug>` ref while `task_branch_name`
/// silently points at a different, often non-existent ref — so push runs
/// against the real branch but `item_done` thinks nothing diverged and
/// returns Ok without push/PR/error (item #331 / #512).
pub fn resolve_item_task_branch(item: &Item, repo_root: &Path) -> String {
    let worktree_path = item_worktree_path(repo_root, item.sequence_id);
    resolve_worktree_branch(item, &worktree_path)
}

/// Creates an isolated git worktree for `item` against `target_branch`.
///
/// Deliberately takes an already-resolved `target_branch` instead of a
/// database connection: callers should resolve the branch (`resolve_target_branch`,
/// above) while still holding whatever lock guards the database, then call
/// this *after* releasing it. `git worktree add` is a blocking
/// filesystem+subprocess operation with no business running while a shared
/// DB lock is held.
pub fn create_worktree(
    item: &Item,
    repo_root: &Path,
    target_branch: &str,
    progress: Option<&dyn Progress>,
) -> Result<PathBuf, String> {
    let worktree_path = item_worktree_path(repo_root, item.sequence_id);
    let label = format!("task-{}", item.sequence_id);
    if worktree_path.is_dir() && !is_own_checkout(&worktree_path) {
        // Broken or missing `.git` pointer: git run inside it would act on
        // the main repo. Relink it if its admin entry survives; otherwise
        // snapshot its contents and clear the directory so a fresh add can
        // take the path.
        let path = worktree_path.to_string_lossy().to_string();
        let _ = run_git_in(repo_root, &["worktree", "repair", &path]);
        if !is_own_checkout(&worktree_path) {
            let name = item.sequence_id.to_string();
            if gc_orphans(repo_root, std::slice::from_ref(&name)).is_empty() {
                return Err(format!(
                    "worktree: {} is not a valid checkout and could not be cleared for item {}",
                    worktree_path.display(),
                    item.id
                ));
            }
        }
    }
    if worktree_path.is_dir() && is_own_checkout(&worktree_path) {
        if is_half_created(&worktree_path) {
            // Holds no work of its own yet; re-add it from scratch below.
            eprintln!(
                "worktree: {} was left half-created by an interrupted `worktree add`, recreating",
                worktree_path.display()
            );
            let path = worktree_path.to_string_lossy().to_string();
            let _ = run_git_in(
                repo_root,
                &["worktree", "remove", "--force", "--force", &path],
            );
            if worktree_path.exists() {
                remove_worktree_dir(&worktree_path, &label);
            }
            remove_stale_registration_for_path(repo_root, &worktree_path);
        } else {
            heal_interrupted_git_state(&worktree_path, false)?;
        }
    }
    let branch = resolve_worktree_branch(item, &worktree_path);
    // Two ways a re-claim can find its own worktree already in place:
    // `already_isolated_for` catches the recursive case (the calling
    // process is itself already running from inside it), but the daemon's
    // normal dispatch always calls this from the main repo root, so that
    // check never fires there -- it needs the on-disk check too, or
    // `git worktree add` below fails ("already exists") on a re-claim.
    if already_isolated_for(&branch, repo_root)
        || worktree_already_checked_out(&worktree_path, &branch)
    {
        // Re-claiming an existing worktree: nothing to create, but still
        // ensure its target dir is isolated (idempotent, no-op if present),
        // and re-warn since the ambient env can still be shadowing it.
        warn_if_ambient_target_dir();
        isolate_worktree_target_dir(&worktree_path);
        lock_item_worktree(repo_root, &worktree_path);
        return Ok(worktree_path);
    }
    // Never silently proceed over an existing checkout holding uncommitted
    // work: a retry racing a half-torn-down worktree must fail loudly rather
    // than clobber it (item #633 ask 4 — #631's rescue was manual).
    if worktree_path.is_dir() && !worktree_already_checked_out(&worktree_path, &branch) {
        // Fail closed: if cleanliness cannot be verified, don't proceed to
        // stale-cleanup + recreate — an unreadable status must never read as
        // "clean".
        let status = run_git_in(worktree_path.as_path(), &["status", "--porcelain"]).map_err(|e| {
            format!(
                "worktree: refusing to recreate at {} for item {} because cleanliness could not be verified: {e}",
                worktree_path.display(),
                item.id
            )
        })?;
        if !status.trim().is_empty() {
            let msg = format!(
                "worktree: refusing to recreate over dirty worktree at {} for item {} \
                 (uncommitted changes preserved; clear or commit them before retrying)",
                worktree_path.display(),
                item.id
            );
            eprintln!("{msg}");
            return Err(msg);
        }
        // Clean, but on a detached HEAD or another branch: `worktree add`
        // can never succeed over the occupied path, so switch it in place.
        if is_own_checkout(&worktree_path) {
            adopt_existing_checkout(&worktree_path, &branch, &label).map_err(|e| {
                format!(
                    "worktree: could not switch existing checkout at {} onto {branch} for item {}: {e}",
                    worktree_path.display(),
                    item.id
                )
            })?;
            warn_if_ambient_target_dir();
            isolate_worktree_target_dir(&worktree_path);
            lock_item_worktree(repo_root, &worktree_path);
            return Ok(worktree_path);
        }
    }
    ensure_worktrees_ignored(repo_root);
    if let Some(parent) = worktree_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    warn_if_ambient_target_dir();
    if let Some(p) = progress {
        p.send(
            0.0,
            Some(1.0),
            Some(format!(
                "Creating isolated worktree for item {}...",
                item.sequence_id
            )),
        );
    }
    let fetch_timeout_secs = 30;
    // `branch` (task/N) can already exist with no worktree owning it -- e.g. a
    // second-session claim on an item whose branch survived from before.
    // `git worktree add -b <branch>` unconditionally fails in that case ("a
    // branch named 'task/N' already exists"), with no recovery: check first,
    // and reuse the existing ref instead of trying to recreate it.
    let branch_ref = format!("refs/heads/{branch}");
    let mut branch_exists = run_git_in_ok(
        repo_root,
        &["rev-parse", "--verify", "--quiet", &branch_ref],
    );
    if !branch_exists {
        // Might exist only on the remote (pushed from another machine/session
        // with no local ref yet) -- fetch it specifically and check again.
        // Soft-fails like every other network step in this file: no remote,
        // offline, or never pushed all fall through to the brand-new-branch
        // path below, unchanged from today's behavior.
        let _ = fetch_with_retry(
            crate::shell::git_binary(),
            &["fetch", "origin", &branch],
            repo_root,
            fetch_timeout_secs,
        );
        branch_exists = run_git_in_ok(
            repo_root,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/remotes/origin/{branch}"),
            ],
        );
    }
    let start_point: Option<String> = if branch_exists {
        // Existing branch (local or now-fetched remote-tracking): it may still
        // be tied to a broken worktree registration from an earlier attempt
        // whose directory was removed out-of-band (crash, manual cleanup) --
        // `worktree_already_checked_out` above only looks at the on-disk
        // directory, so it can't see this case, and `git worktree add` below
        // unconditionally refuses to check out a branch git still considers
        // checked out elsewhere ("already checked out" / prunable registration
        // -- confirmed live on item #331, regenerating on every dispatch
        // attempt without this). Clear that registration first so it never
        // survives to block reuse.
        //
        // Deliberately NOT `git worktree prune`: prune is repo-wide, and it
        // drops the admin entry of ANY worktree whose `gitdir` file points
        // somewhere non-existent -- even one whose directory is fully intact
        // and holds uncommitted work. That turned one item's failed-dispatch
        // retry into another item's data loss: the victim's `.git` pointer
        // became dangling, `audit_orphans` then classified it as a broken-
        // gitdir orphan, and `gc_orphans` deleted it. Scope the cleanup to
        // this branch's own stale registration instead. Also clear by path:
        // a stale entry can point at our path under an old ref even when the
        // branch lookup above resolved differently (item #633).
        remove_stale_registration_for(repo_root, &branch);
        remove_stale_registration_for_path(repo_root, &worktree_path);
        None
    } else {
        // Brand new branch: branch off the freshly-fetched remote ref when
        // reachable, so a stale local checkout (e.g. hasn't pulled a
        // just-merged PR) doesn't silently seed new work from old code.
        // Soft-fails to today's local-ref behavior when there's no remote,
        // we're offline, or the branch was never pushed (common for a parent
        // item's task/N branch) — never blocks a claim on network
        // reachability. Routed through `run_output_timeout` (not the plain
        // blocking `run_git_in`): an unreachable remote or a credential
        // prompt must not be able to hang a claim indefinitely.
        let fetch_result = fetch_with_retry(
            crate::shell::git_binary(),
            &["fetch", "origin", target_branch],
            repo_root,
            fetch_timeout_secs,
        );
        let start_point = match &fetch_result {
            Ok(out)
                if out.status.success()
                    && run_git_in_ok(
                        repo_root,
                        &["rev-parse", "--verify", &format!("origin/{target_branch}")],
                    ) =>
            {
                format!("origin/{target_branch}")
            }
            _ => {
                let reason = match &fetch_result {
                    Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_string(),
                    Err(e) => e.clone(),
                };
                eprintln!(
                    "worktree: could not fetch '{target_branch}' from origin, branching off \
                     the local ref instead ({})",
                    if reason.is_empty() {
                        "no error detail"
                    } else {
                        &reason
                    }
                );
                target_branch.to_string()
            }
        };
        Some(start_point)
    };
    let path_arg = worktree_path.to_string_lossy().to_string();
    // `git worktree add` takes its own lock on `.git/config`/`.git/worktrees`
    // admin state; two calls against the same repo at the same instant race
    // on that lock and the loser fails outright ("could not lock config
    // file .git/config: File exists"). Serialize in-process so concurrent
    // claims from the same daemon (e.g. two items dispatched in the same
    // supervisor tick) queue instead of racing. Deliberately a separate
    // lock from `with_backend_db`'s -- this is a subprocess call, not DB
    // access, and item.rs already runs it outside that lock on purpose.
    //
    // The in-process mutex cannot serialize across separate job processes, and
    // a redispatch can fire while the previous attempt's teardown is still in
    // flight — the registration and the directory disagree for a few seconds
    // ("missing but already registered"). Retry retryable failures with a
    // short backoff so teardown finishes instead of churning fresh dispatches
    // every ~12s (item #633): same job waits, rather than terminal-failing
    // into an immediate redispatch loop.
    const ADD_ATTEMPTS: usize = 3;
    const ADD_BACKOFF_SECS: [u64; 2] = [2, 5];
    let mut attempts_made = 0usize;
    let mut last_err = String::new();
    for attempt in 0..ADD_ATTEMPTS {
        attempts_made = attempt + 1;
        if attempt > 0 {
            // Reconcile again before retrying: the first attempt's failure may
            // have been the stale registration itself, now cleared.
            remove_stale_registration_for(repo_root, &branch);
            remove_stale_registration_for_path(repo_root, &worktree_path);
            std::thread::sleep(std::time::Duration::from_secs(
                ADD_BACKOFF_SECS[(attempt - 1).min(ADD_BACKOFF_SECS.len() - 1)],
            ));
        }
        // Re-derived every attempt: a failed attempt can still have created
        // the branch (e.g. `-b` succeeded, then writing its tracking config
        // lost a lock race), and repeating `-b` would then fail permanently
        // with "a branch named ... already exists".
        let exists_now = branch_exists
            || run_git_in_ok(
                repo_root,
                &["rev-parse", "--verify", "--quiet", &branch_ref],
            );
        let arg_refs: Vec<&str> = match (&start_point, exists_now) {
            // Existing branch: check it out as-is, no `-b` -- git auto-creates
            // the local tracking branch when only the remote-tracking ref
            // exists, same as `git checkout <branch>`.
            (None, _) | (Some(_), true) => vec!["worktree", "add", &path_arg, &branch],
            (Some(start), false) => vec!["worktree", "add", &path_arg, "-b", &branch, start],
        };
        // Bounded: checkout hooks or a huge tree must not hold the add lock
        // (and so every other claim in this process) indefinitely. A killed
        // add leaves a half-created checkout, which the next attempt at the
        // top of this function detects and recreates.
        let git_result = {
            let _guard = WORKTREE_ADD_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            run_git_timeout(repo_root, &arg_refs, WORKTREE_ADD_TIMEOUT_SECS)
        };
        match git_result {
            Ok(_) => {
                if let Some(p) = progress {
                    p.send(1.0, Some(1.0), Some("Worktree created".into()));
                }
                isolate_worktree_target_dir(&worktree_path);
                lock_item_worktree(repo_root, &worktree_path);
                return Ok(worktree_path);
            }
            Err(e) => {
                last_err = e;
                if !is_retryable_worktree_race(&last_err) {
                    break;
                }
            }
        }
    }
    let msg = format!(
        "worktree: creation skipped for item {} after {attempts_made} attempt(s): {}",
        item.id, last_err
    );
    eprintln!("{msg}");
    Err(msg)
}

/// Removes the stale `.git/worktrees/<name>` admin entry that claims
/// `branch`, if there is one. Returns whether anything was removed.
///
/// This is the narrow, per-branch equivalent of `git worktree prune`, and
/// exists because prune's blast radius is the whole repo. Prune deletes the
/// admin entry of *every* registration whose `gitdir` file points at a
/// missing path — including a worktree that is still fully present on disk
/// with uncommitted work in it, whose admin entry merely went stale (a
/// moved checkout, an interrupted operation, or a Windows path/locking
/// hiccup). The victim is left with a dangling `.git` pointer, which
/// `audit_orphans` reads as "broken gitdir" and `gc_orphans` then deletes.
///
/// Two guards keep this scoped: only registrations whose `HEAD` names
/// `branch` are considered, and only ones whose checkout directory is
/// actually gone. A registration pointing at a live directory is never
/// touched, so another item's worktree can never be collateral damage.
fn remove_stale_registration_for(repo_root: &Path, branch: &str) -> bool {
    let Ok(common_dir) = run_git_in(repo_root, &["rev-parse", "--git-common-dir"]) else {
        return false;
    };
    let admin_root = repo_root.join(common_dir.trim()).join("worktrees");
    let Ok(entries) = std::fs::read_dir(&admin_root) else {
        return false;
    };
    let wanted_head = format!("ref: refs/heads/{branch}");
    let mut removed = false;
    for entry in entries.flatten() {
        let admin = entry.path();
        if !admin.is_dir() {
            continue;
        }
        // Does this registration claim our branch?
        let head = std::fs::read_to_string(admin.join("HEAD")).unwrap_or_default();
        if head.trim() != wanted_head || !lock_is_ours_or_absent(&admin) {
            continue;
        }
        // `gitdir` holds "<checkout>/.git" -- its parent is the checkout.
        // Preserve the registration if that directory still exists (or if
        // the file is unreadable): fail closed, since removing a live
        // worktree's registration is the exact harm this function avoids.
        let Ok(gitdir) = std::fs::read_to_string(admin.join("gitdir")) else {
            continue;
        };
        let still_live = resolve_gitdir_pointer(&admin, &gitdir)
            .parent()
            .is_none_or(std::path::Path::exists);
        if still_live {
            continue;
        }
        if std::fs::remove_dir_all(&admin).is_ok() {
            removed = true;
        }
    }
    removed
}

/// Removes the stale `.git/worktrees/<name>` admin entry pointing at
/// `worktree_path`, if that directory is genuinely gone. Path-scoped
/// companion to `remove_stale_registration_for` above: the branch-scoped
/// cleaner cannot see a stale entry when the branch itself doesn't exist yet
/// (brand-new-branch path) or still names an old ref, yet `git worktree add`
/// still refuses with "missing but already registered" for that path.
///
/// Same fail-closed guards, scoped the other way: only entries whose `gitdir`
/// parent resolves to exactly `worktree_path` are considered, and only when
/// that directory is actually absent — verified twice with a short pause
/// between checks so a teardown still in flight isn't mistaken for genuinely
/// gone (item #633). Never touches a live directory.
fn remove_stale_registration_for_path(repo_root: &Path, worktree_path: &Path) -> bool {
    let Ok(common_dir) = run_git_in(repo_root, &["rev-parse", "--git-common-dir"]) else {
        return false;
    };
    let admin_root = repo_root.join(common_dir.trim()).join("worktrees");
    let Ok(entries) = std::fs::read_dir(&admin_root) else {
        return false;
    };
    let mut removed = false;
    for entry in entries.flatten() {
        let admin = entry.path();
        if !admin.is_dir() {
            continue;
        }
        // Never touch a locked registration (`git worktree lock` pins the
        // entry on purpose — e.g. portable/network paths).
        if !lock_is_ours_or_absent(&admin) {
            continue;
        }
        let gitdir = std::fs::read_to_string(admin.join("gitdir")).unwrap_or_default();
        let checkout = resolve_gitdir_pointer(&admin, &gitdir)
            .parent()
            .map(std::path::Path::to_path_buf);
        let is_ours = checkout
            .as_deref()
            .is_some_and(|c| same_location(c, worktree_path));
        if !is_ours {
            continue;
        }
        // Double-check with a beat in between: teardown in flight can make a
        // live directory look briefly absent.
        if worktree_path.exists() {
            continue;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        if worktree_path.exists() {
            continue;
        }
        // Re-verify identity just before removal: another process may have
        // recycled this admin dir for a replacement worktree since we first
        // read it — deleting that live registration would orphan real work.
        let gitdir_again = std::fs::read_to_string(admin.join("gitdir")).unwrap_or_default();
        let still_ours = resolve_gitdir_pointer(&admin, &gitdir_again)
            .parent()
            .is_some_and(|c| same_location(c, worktree_path));
        if !still_ours || !lock_is_ours_or_absent(&admin) {
            continue;
        }
        if std::fs::remove_dir_all(&admin).is_ok() {
            removed = true;
        }
    }
    removed
}

/// Whether `branch` has committed content `target_branch` doesn't already
/// have: new commits (`rev-list --count`) whose *content* isn't already
/// fully present on the target either (`diff --quiet` content-diff guard,
/// catching squash-merges — two-dot, not three-dot: we want whether the two
/// tips are identical, not whether branch differs from merge-base).
///
/// Used by `push_branch` below to skip pushing/PR-ing a no-op branch, and by
/// `item_done` (main binary) to tell a genuinely empty run (no commits at
/// all) apart from a run whose push/PR failed for some other reason —
/// only the former should block marking an item done.
pub fn branch_diverged(repo_root: &Path, branch: &str, target_branch: &str) -> bool {
    // Both the local target and `origin/<target>` count as "already there":
    // the daemon never pulls, so the local ref alone goes stale (upstream
    // commits misread as the branch's own), and a target that only exists
    // remotely (a parent item's pushed branch) must not read as "nothing
    // committed" just because the local ref is missing.
    let bases: Vec<String> = [
        target_branch.to_string(),
        format!("refs/remotes/origin/{target_branch}"),
    ]
    .into_iter()
    .filter(|r| run_git_in_ok(repo_root, &["rev-parse", "--verify", "--quiet", r]))
    .collect();
    if bases.is_empty() {
        return false;
    }
    let mut rev_list = vec!["rev-list", "--count", branch, "--not"];
    rev_list.extend(bases.iter().map(String::as_str));
    match run_git_in(repo_root, &rev_list) {
        Ok(count) if count != "0" => {}
        _ => return false,
    }
    // Squash-merge guard: content identical to either base means landed.
    !bases.iter().any(|base| {
        run_git_in_ok(
            repo_root,
            &["diff", "--quiet", &format!("{base}..{branch}")],
        )
    })
}

/// Pushes `item`'s isolated worktree branch to `target_branch`'s remote, if
/// the branch exists, has new commits, and its content isn't already fully
/// present on the target (squash-merge guard). Returns the pushed branch
/// name on success. Soft-fails (eprintln, no error surfaced, returns
/// `None`) on any failure — nothing here should block `done` since the
/// item's completion is already committed to the DB by the time this runs.
///
/// Rebases onto `target_branch`'s latest tip (via [`rebase_item_worktree`])
/// before pushing — the work session between claim and `done` can easily
/// outlive other agents' merges to `target_branch`, so without this the PR
/// opened next can already be behind the base it's meant to land on (item
/// #161). A conflict there just aborts the rebase and pushes the branch
/// unchanged, same as if this call weren't here at all.
///
/// Pushes with `--force-with-lease` rather than a plain push: a successful
/// rebase rewrites this branch's commits, so a plain push to a branch this
/// process already pushed in an earlier `done` call would be rejected as
/// non-fast-forward. `--force-with-lease` only overwrites the remote branch
/// if it still matches what this process last saw (its own remote-tracking
/// ref, refreshed by the rebase's own fetch above), so it's safe even when
/// nothing was actually rebased this call.
///
/// Deliberately does NOT open a PR — that's a GitHub-API concern kept out
/// of this crate; see the thin wrapper in the main binary's
/// `src/worktree.rs::push_and_open_pr`.
pub fn push_branch(
    item: &Item,
    repo_root: &Path,
    target_branch: &str,
    progress: Option<&dyn Progress>,
) -> Option<String> {
    let worktree_path = item_worktree_path(repo_root, item.sequence_id);
    if !worktree_path.exists() {
        return None; // nothing was ever claimed into a worktree for this item
    }
    match rebase_item_worktree(item, repo_root, target_branch) {
        RebaseOutcome::Rebased => {
            eprintln!(
                "worktree: rebased item {}'s branch onto latest {target_branch} before push",
                item.id
            );
        }
        RebaseOutcome::Conflict(detail) => {
            eprintln!(
                "worktree: rebase onto latest {target_branch} conflicted for item {} \
                 (pushing the branch unchanged instead): {detail}",
                item.id
            );
        }
        RebaseOutcome::Skipped | RebaseOutcome::UpToDate | RebaseOutcome::Dirty => {}
    }
    let branch = resolve_item_task_branch(item, repo_root);
    // Nothing to push (and nothing worth a PR) if the branch never
    // diverged from its target — e.g. `done` called with no commits made.
    if !branch_diverged(repo_root, &branch, target_branch) {
        return None;
    }
    if let Some(p) = progress {
        p.send(0.0, Some(1.0), Some(format!("Pushing branch {branch}...")));
    }
    let first = push_with_lease(repo_root, &branch);
    let result = match first {
        Err(e) if is_push_rejection(&e) => {
            // Someone else (a human fixup, GitHub's update-branch, another
            // machine) put commits on the remote branch this worktree never
            // integrated. Integrate them rather than overwrite them -- or
            // stay rejected forever, since every later rebase rewrites the
            // local branch again.
            match integrate_remote_branch(&worktree_path, &branch) {
                Ok(()) => push_with_lease(repo_root, &branch),
                Err(detail) => Err(format!(
                    "{e} (integrating origin/{branch} failed: {detail})"
                )),
            }
        }
        other => other,
    };
    match result {
        Ok(()) => Some(branch),
        Err(e) => {
            eprintln!("worktree: push failed for item {}: {e}", item.id);
            None
        }
    }
}

/// Outcome of [`rebase_item_worktree`].
pub enum RebaseOutcome {
    /// No worktree exists yet, or the fetch itself didn't land (no remote,
    /// offline, or the fetched ref didn't resolve) -- soft-failed like
    /// every other network step in this file rather than blocking the
    /// caller.
    Skipped,
    /// The worktree's branch already contains the fetched target tip --
    /// nothing to rebase.
    UpToDate,
    /// Rebased cleanly onto the fetched target tip.
    Rebased,
    /// The worktree had uncommitted changes -- rebasing over them risks
    /// losing or corrupting in-progress work, so nothing ran.
    Dirty,
    /// The rebase hit a conflict and was aborted; the branch is exactly as
    /// it was before this call. Carries `git rebase`'s stderr for whoever
    /// surfaces this to a human.
    Conflict(String),
}

/// Fetches the current tip of `target_branch` and rebases `item`'s worktree
/// branch onto it, if the worktree exists and is clean.
///
/// `create_worktree`'s own fetch only ever runs once, at initial creation --
/// a worktree that's re-claimed/redispatched, or that simply sits idle while
/// other agents merge unrelated PRs into `target_branch`, never gets
/// refreshed, so work (and the PR eventually opened from it) can proceed
/// against an already-stale base. Callers are expected to invoke this both
/// right after a claim resolves a worktree (covering the re-claim/redispatch
/// case) and again right before opening a PR (covering drift that happened
/// during the work session itself) (item #161).
///
/// Never forces or discards anything: a dirty tree is left untouched
/// (`Dirty`), and a real conflict aborts the rebase immediately (`Conflict`)
/// rather than leaving the worktree mid-rebase or resolving anything
/// automatically. Soft-fails (`Skipped`) on no worktree / no reachable
/// remote, matching every other network step in this file.
pub fn rebase_item_worktree(item: &Item, repo_root: &Path, target_branch: &str) -> RebaseOutcome {
    let worktree_path = item_worktree_path(repo_root, item.sequence_id);
    if !worktree_path.is_dir() {
        return RebaseOutcome::Skipped;
    }
    if let Err(e) = heal_interrupted_git_state(&worktree_path, false) {
        return RebaseOutcome::Conflict(e);
    }
    let fetch_timeout_secs = 30;
    let fetch_result = fetch_with_retry(
        crate::shell::git_binary(),
        &["fetch", "origin", target_branch],
        &worktree_path,
        fetch_timeout_secs,
    );
    let remote_ref = format!("origin/{target_branch}");
    let fetched = matches!(&fetch_result, Ok(out) if out.status.success())
        && run_git_in_ok(
            &worktree_path,
            &["rev-parse", "--verify", "--quiet", &remote_ref],
        );
    if !fetched {
        return RebaseOutcome::Skipped;
    }
    match run_git_in(&worktree_path, &["status", "--porcelain"]) {
        Ok(out) if out.trim().is_empty() => {}
        _ => return RebaseOutcome::Dirty,
    }
    if run_git_in_ok(
        &worktree_path,
        &["merge-base", "--is-ancestor", &remote_ref, "HEAD"],
    ) {
        return RebaseOutcome::UpToDate;
    }
    match run_git_timeout(
        &worktree_path,
        &["rebase", &remote_ref],
        REBASE_TIMEOUT_SECS,
    ) {
        Ok(_) => RebaseOutcome::Rebased,
        Err(e) => {
            // A timed-out rebase was killed while holding `index.lock`, so
            // the lock is ours to clear before `--abort` can run; checked,
            // not discarded -- a failed abort leaves the worktree wedged.
            match heal_interrupted_git_state(&worktree_path, true) {
                Ok(()) => RebaseOutcome::Conflict(e),
                Err(stuck) => RebaseOutcome::Conflict(format!("{e}; {stuck}")),
            }
        }
    }
}

const REBASE_TIMEOUT_SECS: u64 = 60;

/// Removes `item`'s own worktree once its work is done, if it's safe to.
///
/// `create_worktree` provisions a directory per item but nothing ever
/// removed it (item #420) — every completed item left an orphaned
/// `.worktrees/task/<id>` behind, and the git shim blocks removing one by
/// hand. Deleting a linked worktree's directory only drops the checkout,
/// never the branch or its commits (those live in the shared object store),
/// so the one real risk is uncommitted changes that exist only in that
/// checkout. Refuses (logs, returns `false`) on a dirty tree or a failed
/// status check rather than guessing from push/PR outcome — a caller can
/// mark an item done without ever pushing, and deleting then would be
/// destructive.
pub fn cleanup_item_worktree(item: &Item, repo_root: &Path) -> bool {
    let name = item.sequence_id.to_string();
    let worktree_path = item_worktree_path(repo_root, item.sequence_id);
    if !worktree_path.exists() {
        return false;
    }
    if !is_own_checkout(&worktree_path) {
        eprintln!(
            "worktree: leaving {} in place -- not a valid checkout",
            worktree_path.display()
        );
        return false;
    }
    // Commits made on a detached HEAD live only in this checkout's reflog.
    rescue_detached_head(&worktree_path, &format!("task-{name}"));
    match run_git_in(&worktree_path, &["status", "--porcelain"]) {
        Ok(out) if out.trim().is_empty() => {}
        Ok(_) => {
            eprintln!(
                "worktree: leaving {} in place — it has uncommitted changes",
                worktree_path.display()
            );
            return false;
        }
        Err(e) => {
            eprintln!(
                "worktree: leaving {} in place — could not check its status: {e}",
                worktree_path.display()
            );
            return false;
        }
    }
    !gc_orphans(repo_root, &[name]).is_empty()
}

/// Result of `commit_uncommitted` -- distinguishes "there was nothing to
/// commit" from "there was something to commit and it failed", which a bare
/// `bool` couldn't (item #92). A permissions failure (item #88's read-only
/// `.git` under bwrap) or any similar future failure mode -- disk full, a
/// pre-commit hook rejection, a git config issue -- used to look identical
/// to a genuine no-op from the caller's side, so `item_done` reported
/// success while real, staged work sat stranded uncommitted.
pub enum CommitOutcome {
    /// The worktree doesn't exist, or its tree was already clean -- nothing
    /// to commit, not a failure.
    NothingToCommit,
    /// Uncommitted changes existed and were committed successfully.
    Committed,
    /// Uncommitted changes existed but `git add`/`git commit` failed --
    /// real work is stranded in the worktree, still uncommitted.
    Failed(String),
}

/// Commits any uncommitted changes sitting in `item`'s worktree checkout.
///
/// An agent can make real file edits and still exit without ever running
/// `git commit` itself -- with no commit, the branch never diverges from
/// its target, so `item_done` (main binary) can't tell that apart from a
/// genuine no-op and the edits are silently stranded while `done` reports
/// success (item #57). Called before that divergence check runs, so a
/// forgotten commit gets made here first instead of falling through to the
/// "nothing was committed" path.
///
/// Returns `NothingToCommit` when the worktree doesn't exist or is already
/// clean -- the caller's existing dirty-tree handling (`cleanup_item_worktree`
/// refusing to remove it) still covers those exactly as it did before. Once
/// `status` has confirmed there IS something to commit, an `add`/`commit`
/// failure is reported as `Failed` rather than silently folded into the same
/// "nothing to do" bucket.
pub fn commit_uncommitted(item: &Item, repo_root: &Path, message: &str) -> CommitOutcome {
    let worktree_path = item_worktree_path(repo_root, item.sequence_id);
    commit_uncommitted_at(&worktree_path, message, false)
}

/// Same mechanics as `commit_uncommitted`, but against an already-resolved
/// worktree path instead of one derived from `Item::sequence_id` -- used by
/// the SDD loop's per-implementer-turn checkpoint commits (item #193),
/// where the caller already holds the path via `WorkItemData::worktree_path`
/// rather than an `Item`.
///
/// `no_verify` skips local git hooks, including the repo's LOC-freeze
/// pre-commit gate (`scripts/loc-gate.sh`, item #141) -- a checkpoint commit
/// intentionally bypasses it, since an in-progress turn's diff can
/// legitimately, if only temporarily, exceed the gate; `finalize`'s
/// squashed commit (see `squash_since`) is what the gate actually evaluates.
pub fn commit_uncommitted_at(
    worktree_path: &Path,
    message: &str,
    no_verify: bool,
) -> CommitOutcome {
    if !worktree_path.is_dir() {
        return CommitOutcome::NothingToCommit;
    }
    // A checkout with a broken `.git` pointer would otherwise commit into
    // the enclosing main repository's current branch.
    if !is_own_checkout(worktree_path) {
        return CommitOutcome::Failed(format!(
            "{} is not a valid git checkout",
            worktree_path.display()
        ));
    }
    if let Err(e) = heal_interrupted_git_state(worktree_path, false) {
        return CommitOutcome::Failed(e);
    }
    match run_git_in(worktree_path, &["status", "--porcelain"]) {
        Ok(out) if !out.trim().is_empty() => {}
        _ => return CommitOutcome::NothingToCommit,
    }
    if let Err(e) = run_git_in(worktree_path, &["add", "-A"]) {
        return CommitOutcome::Failed(format!("git add failed: {e}"));
    }
    let mut args = vec!["commit", "-m", message];
    if no_verify {
        args.push("--no-verify");
    }
    match run_git_in(worktree_path, &args) {
        Ok(_) => CommitOutcome::Committed,
        Err(e) => CommitOutcome::Failed(format!("git commit failed: {e}")),
    }
}

/// The worktree's current `HEAD` commit SHA, if resolvable -- used to
/// capture the SDD loop's pre-checkpoint base (item #193) before its first
/// per-turn commit, so `finalize` can later fold every checkpoint commit
/// back into an uncommitted diff via `squash_since`.
pub fn head_sha(worktree_path: &Path) -> Option<String> {
    run_git_in(worktree_path, &["rev-parse", "HEAD"]).ok()
}

/// Folds every commit made since `base_sha` back into the index and working
/// tree as one uncommitted diff, without touching any file content --
/// `git reset --soft` under the hood. Used by `finalize` (item #193) to
/// squash the SDD loop's per-turn checkpoint commits into a single diff
/// right before its own `commit_uncommitted` call, so the repo's LOC-freeze
/// gate evaluates the whole run's changes as one commit instead of turn by
/// turn. A no-op if `base_sha` is already `HEAD`.
pub fn squash_since(worktree_path: &Path, base_sha: &str) -> Result<(), String> {
    // After an intervening rebase `base_sha` is no longer in HEAD's history;
    // a soft reset to it would fold every upstream change since into this
    // item's diff.
    if !run_git_in_ok(
        worktree_path,
        &["merge-base", "--is-ancestor", base_sha, "HEAD"],
    ) {
        return Err(format!(
            "squash base {base_sha} is not an ancestor of HEAD (branch was rebased)"
        ));
    }
    run_git_in(worktree_path, &["reset", "--soft", base_sha]).map(|_| ())
}

#[cfg(test)]
#[path = "worktree_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "worktree_heal_tests.rs"]
mod heal_tests;
