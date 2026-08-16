//! Worktree lifecycle management: isolates work items into per-branch git
//! worktrees, resolves target branches from parent item metadata, and keeps
//! each worktree's Cargo target dir isolated.
//!
//! GitHub-API concerns (opening a PR once a branch is pushed) are
//! deliberately NOT here — `push_branch` below only handles the local push
//! mechanics and returns the pushed branch name; opening the PR is the
//! caller's job (see the thin wrapper in the main binary's
//! `src/worktree.rs`), so this crate stays free of any GitHub dependency.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use agentflare_backend::item::Item;

use crate::branch::resolve_default_branch;
use crate::shell::{run_in as run_git_in, run_in_ok as run_git_in_ok};

/// Minimal progress-reporting interface — decouples this crate from the
/// main binary's MCP-specific `ProgressSender` (which depends on `rmcp`),
/// so this leaf crate has no reason to know anything about MCP.
pub trait Progress {
    fn send(&self, progress: f64, total: Option<f64>, message: Option<String>);
}

pub fn resolve_target_branch(conn: &rusqlite::Connection, item: &Item, repo_root: &Path) -> String {
    if let Some(ref parent_id) = item.parent_id
        && let Ok(parent) = agentflare_backend::item::get(conn, parent_id)
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
    Command::new("sccache")
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

/// The branch `item`'s worktree is (or should be) on: whatever a worktree
/// already sitting at `worktree_path` is *actually* checked out to, falling
/// back to the freshly-derived `task_branch_name` only when no worktree
/// exists there yet.
///
/// `task_branch_name` recomputes its slug from `item.name` every call, so it
/// silently changes if the item gets renamed between claims -- and a
/// worktree created before this slugged naming scheme existed simply sits on
/// the bare `task/<sequence_id>`. Either way, re-deriving a slug that
/// doesn't match what's already checked out makes `create_worktree`'s
/// re-claim detection miss the existing worktree, so `git worktree add`
/// collides with the already-occupied path instead of reusing it (item
/// #459's dispatch-blocking loop on item #447, which predates the slugged
/// scheme).
fn resolve_worktree_branch(item: &Item, worktree_path: &Path) -> String {
    if worktree_path.is_dir()
        && let Ok(current) = run_git_in(worktree_path, &["branch", "--show-current"])
        && !current.is_empty()
        && (current == format!("task/{}", item.sequence_id)
            || current.starts_with(&format!("task/{}-", item.sequence_id)))
    {
        return current;
    }
    task_branch_name(item)
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
    let worktree_path = repo_root
        .join(".worktrees")
        .join("task")
        .join(item.sequence_id.to_string());
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
        return Ok(worktree_path);
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
    let worktree_add_args: Vec<String> = if branch_exists {
        // Existing branch (local or now-fetched remote-tracking): check it out
        // as-is, no `-b` -- git auto-creates the local tracking branch when
        // only the remote-tracking ref exists, same as `git checkout <branch>`.
        vec![
            "worktree".to_string(),
            "add".to_string(),
            worktree_path.to_string_lossy().to_string(),
            branch.clone(),
        ]
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
        vec![
            "worktree".to_string(),
            "add".to_string(),
            worktree_path.to_string_lossy().to_string(),
            "-b".to_string(),
            branch.clone(),
            start_point,
        ]
    };
    let arg_refs: Vec<&str> = worktree_add_args.iter().map(String::as_str).collect();
    match run_git_in(repo_root, &arg_refs) {
        Ok(_) => {
            if let Some(p) = progress {
                p.send(1.0, Some(1.0), Some("Worktree created".into()));
            }
            isolate_worktree_target_dir(&worktree_path);
            Ok(worktree_path)
        }
        Err(e) => {
            let msg = format!("worktree: creation skipped for item {}: {}", item.id, e);
            eprintln!("{msg}");
            Err(msg)
        }
    }
}

/// Kills `child` and its whole process tree — not just the direct child —
/// so a grandchild (e.g. a `git` credential helper) can't outlive a timeout.
fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // `kill -KILL -<pid>` packs the signal and the (negative, i.e.
        // process-group-targeting) pid into two separate `-`-prefixed argv
        // entries. Some `kill` implementations misparse the second as
        // another option rather than as the target once a signal option has
        // already been consumed. `-s SIGNAME` plus a `--` end-of-options
        // marker before the pid is the portable, unambiguous idiom.
        let _ = Command::new("kill")
            .arg("-s")
            .arg("KILL")
            .arg("--")
            .arg(format!("-{}", child.id()))
            .status();
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .status();
        // `taskkill /T` builds its kill list from a single point-in-time
        // process-tree snapshot. A grandchild spawned in the narrow window
        // between that snapshot and termination (e.g. this child hadn't yet
        // exec'd its own subprocess) can survive the call entirely
        // undetected (item #78). Windows keeps a dead process's original
        // parent-PID association around for lookups until the PID is
        // reused, so a second pass a moment later still finds and kills any
        // such straggler; it's a harmless no-op once the tree is already
        // gone.
        std::thread::sleep(Duration::from_millis(250));
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &child.id().to_string()])
            .status();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = child.kill();
    }
}

/// Runs `program` with a deadline, returning its output. Puts the child in
/// its own process group (Unix) and kills that whole group — not just the
/// direct child — if it outlives `timeout_secs`, via `kill_tree`; a plain
/// `child.kill()` would leave a grandchild (e.g. a `git` credential helper)
/// running and the process genuinely un-reaped, not just "late". Stdout/
/// stderr are drained on separate threads so a child that fills an OS pipe
/// buffer can't deadlock the wait loop.
pub(crate) fn run_output_timeout(
    program: impl AsRef<std::ffi::OsStr>,
    args: &[&str],
    cwd: &Path,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    let program = program.as_ref().to_owned();
    let mut cmd = Command::new(&program);
    cmd.args(args)
        .current_dir(cwd)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{}: spawn failed: {e}", program.to_string_lossy()))?;
    let mut stdout_pipe = child.stdout.take().expect("stdout piped above");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped above");
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout_pipe.read_to_end(&mut buf);
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr_pipe.read_to_end(&mut buf);
        buf
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    kill_tree(&mut child);
                    let _ = child.wait();
                    return Err(format!(
                        "{}: timed out after {timeout_secs}s",
                        program.to_string_lossy()
                    ));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("{}: {e}", program.to_string_lossy())),
        }
    };
    Ok(std::process::Output {
        status,
        stdout: stdout_reader.join().unwrap_or_default(),
        stderr: stderr_reader.join().unwrap_or_default(),
    })
}

/// Fixed pause between the two attempts in [`fetch_with_retry`] — long enough
/// to ride out a one-off DNS/TCP blip, short enough not to meaningfully delay
/// a claim on top of `fetch_timeout_secs`.
const FETCH_RETRY_DELAY: Duration = Duration::from_secs(2);

/// [`run_output_timeout`], retried once on failure (a non-zero exit counts as
/// a failure worth retrying, same as a spawn/timeout error).
///
/// The fetches this wraps run once per worktree creation, on the critical
/// path of claiming a work item, against a remote this process doesn't
/// control the reachability of — a single dropped DNS query or transient TCP
/// reset (observed on real workstations, not hypothetical) would otherwise
/// silently seed a new branch off a stale local ref, exactly the staleness
/// this fetch exists to prevent. One retry is deliberately not a full
/// exponential-backoff loop: `fetch_timeout_secs` already bounds each
/// attempt, so retrying more still can't hang a claim indefinitely, but it
/// isn't worth adding unbounded delay chasing a remote that is genuinely
/// unreachable — the caller's existing fallback-to-local-ref handles that
/// case correctly, it just needs to be logged instead of silent.
fn fetch_with_retry(
    program: impl AsRef<std::ffi::OsStr>,
    args: &[&str],
    cwd: &Path,
    timeout_secs: u64,
) -> Result<std::process::Output, String> {
    let program = program.as_ref().to_owned();
    let first = run_output_timeout(&program, args, cwd, timeout_secs);
    if matches!(&first, Ok(out) if out.status.success()) {
        return first;
    }
    std::thread::sleep(FETCH_RETRY_DELAY);
    run_output_timeout(&program, args, cwd, timeout_secs)
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
    match run_git_in(
        repo_root,
        &["rev-list", "--count", &format!("{target_branch}..{branch}")],
    ) {
        Ok(count) if count != "0" => {}
        _ => return false,
    }
    !run_git_in_ok(
        repo_root,
        &["diff", "--quiet", &format!("{target_branch}..{branch}")],
    )
}

/// Pushes `item`'s isolated worktree branch to `target_branch`'s remote, if
/// the branch exists, has new commits, and its content isn't already fully
/// present on the target (squash-merge guard). Returns the pushed branch
/// name on success. Soft-fails (eprintln, no error surfaced, returns
/// `None`) on any failure — nothing here should block `done` since the
/// item's completion is already committed to the DB by the time this runs.
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
    let worktree_path = repo_root
        .join(".worktrees")
        .join("task")
        .join(item.sequence_id.to_string());
    if !worktree_path.exists() {
        return None; // nothing was ever claimed into a worktree for this item
    }
    let branch = resolve_worktree_branch(item, &worktree_path);
    // Nothing to push (and nothing worth a PR) if the branch never
    // diverged from its target — e.g. `done` called with no commits made.
    if !branch_diverged(repo_root, &branch, target_branch) {
        return None;
    }
    if let Some(p) = progress {
        p.send(0.0, Some(1.0), Some(format!("Pushing branch {branch}...")));
    }
    let push_timeout = 120;
    match run_output_timeout(
        crate::shell::git_binary(),
        &["push", "-u", "origin", &branch],
        repo_root,
        push_timeout,
    ) {
        Ok(out) if !out.status.success() => {
            eprintln!(
                "worktree: push skipped for item {}: {}",
                item.id,
                String::from_utf8_lossy(&out.stderr).trim()
            );
            return None;
        }
        Err(e) => {
            eprintln!("worktree: push skipped for item {}: {e}", item.id);
            return None;
        }
        _ => {}
    }
    Some(branch)
}

/// Information about an orphaned worktree detected during audit.
pub struct OrphanWorktree {
    pub name: String,
    pub path: PathBuf,
    pub sequence_id: Option<i64>,
    pub size_bytes: u64,
    pub has_broken_gitdir: bool,
    /// The worktree's gitdir is intact but it sits on the repo's default
    /// branch (a task worktree stranded there) -- the gh merge-collision
    /// case. Mutually exclusive-ish with `has_broken_gitdir`: at least one
    /// of the two is always true for an orphan.
    pub on_default_branch: bool,
}

/// Scan `.worktrees/task/*` for orphaned worktree directories.
///
/// A worktree is considered orphaned when its `.git` gitdir pointer is
/// broken (the file exists but points to a path that no longer exists).
/// This typically means `git worktree remove` was interrupted or the
/// repository's `.git/worktrees/<name>` metadata was manually cleaned
/// up without removing the worktree's working directory.
///
/// When `claimed_item_ids` is provided, any directory whose name matches
/// a live item's sequence_id is excluded from the orphan list — that
/// worktree may simply need a metadata fix rather than removal.
pub fn audit_orphans(
    repo_root: &Path,
    claimed_item_ids: Option<&std::collections::HashSet<String>>,
) -> Vec<OrphanWorktree> {
    let task_dir = repo_root.join(".worktrees").join("task");
    if !task_dir.exists() {
        return Vec::new();
    }
    let default_branch = crate::branch::resolve_default_branch(repo_root);
    let mut orphans = Vec::new();
    // filter_map skips unreadable entries individually rather than failing
    // the whole scan on the first error -- a single permission-denied or
    // transient FS error on one subdirectory must not hide real orphans
    // sitting alongside it.
    let entries: Vec<_> = walkdir::WalkDir::new(&task_dir)
        .max_depth(1)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .collect();
    for entry in entries.iter().skip(1) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let dir_name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let dot_git = path.join(".git");
        let has_broken_gitdir = if dot_git.is_file() {
            match std::fs::read_to_string(&dot_git) {
                Ok(content) => {
                    let gitdir = content.trim().trim_start_matches("gitdir: ");
                    !std::path::Path::new(gitdir).exists()
                }
                Err(_) => false,
            }
        } else {
            false
        };
        // Exclude directories whose name matches a live claimed item
        if let Some(claimed) = claimed_item_ids
            && claimed.contains(&dir_name)
        {
            continue;
        }
        // Only consider as orphan when the gitdir pointer is broken, OR the
        // worktree is sitting on the repo's default branch. The second case
        // is the stranded-worktree root cause behind gh pr merge --delete-
        // branch / post-merge local-sync failures (item #441, vents #351/
        // #394/#423): a task worktree should only ever be on its own
        // task/<N> branch, so one that has been switched to the default
        // branch is abandoned junk -- it holds the default branch checked
        // out, which blocks both deleting it and gh's merge flow.
        let mut on_default_branch = false;
        if !has_broken_gitdir {
            on_default_branch = crate::branch::current_branch(path)
                .map(|b| !b.is_empty() && b == default_branch)
                .unwrap_or(false);
            if !on_default_branch {
                continue;
            }
            // Stranded-but-dirty is still real work -- same guard
            // `cleanup_item_worktree` applies before its own `gc_orphans`
            // call; fail closed (preserve) on a status-check error too.
            let clean = matches!(run_git_in(path, &["status", "--porcelain"]), Ok(o) if o.trim().is_empty());
            if !clean {
                eprintln!(
                    "worktree: preserving {} -- uncommitted changes",
                    path.display()
                );
                continue;
            }
        }
        let sequence_id = dir_name.parse::<i64>().ok();
        let size_bytes = dir_size(path);
        orphans.push(OrphanWorktree {
            name: dir_name,
            path: path.to_path_buf(),
            sequence_id,
            size_bytes,
            has_broken_gitdir,
            on_default_branch,
        });
    }
    orphans
}

/// Delete orphaned worktrees: snapshot first, then remove with retry.
///
/// Takes a list of worktree names (as returned by `audit_orphans`) and
/// snapshots each one before removing it. Removal uses `remove_worktree_dir`
/// which retries with exponential backoff on Windows, falls back to `cmd /c
/// rmdir`, and reports locking processes via handle64.exe when present.
/// Returns the names actually deleted.
pub fn gc_orphans(repo_root: &Path, names: &[String]) -> Vec<String> {
    let mut deleted = Vec::new();
    for name in names {
        let worktree_path = repo_root.join(".worktrees").join("task").join(name);
        if !worktree_path.exists() {
            continue;
        }
        // Snapshot before deletion -- abort this orphan's removal if the
        // snapshot itself fails, since that snapshot is the only recovery
        // point a destructive delete has; deleting anyway would defeat the
        // whole point of snapshotting first.
        let reason = format!("gc orphan worktree {}", name);
        if let Err(e) = crate::snapshot::snapshot_before(repo_root, &reason) {
            eprintln!(
                "worktree: skipping orphan '{}', snapshot failed: {}",
                name, e
            );
            continue;
        }
        if remove_worktree_dir(&worktree_path, name) {
            deleted.push(name.clone());
        }
    }
    // Prune git's worktree metadata after removal.
    crate::shell::prune_worktree_metadata_if(repo_root, !deleted.is_empty());
    deleted
}

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
    let worktree_path = repo_root.join(".worktrees").join("task").join(&name);
    if !worktree_path.exists() {
        return false;
    }
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
    let worktree_path = repo_root
        .join(".worktrees")
        .join("task")
        .join(item.sequence_id.to_string());
    match run_git_in(&worktree_path, &["status", "--porcelain"]) {
        Ok(out) if !out.trim().is_empty() => {}
        _ => return CommitOutcome::NothingToCommit,
    }
    if let Err(e) = run_git_in(&worktree_path, &["add", "-A"]) {
        return CommitOutcome::Failed(format!("git add failed: {e}"));
    }
    match run_git_in(&worktree_path, &["commit", "-m", message]) {
        Ok(_) => CommitOutcome::Committed,
        Err(e) => CommitOutcome::Failed(format!("git commit failed: {e}")),
    }
}

/// Remove a worktree directory, with retry + Windows fallback.
///
/// On Windows, background processes (rust-analyzer, proc-macro-srv) may
/// hold file handles that block `remove_dir_all` with Permission denied.
/// Retries with exponential backoff, falls back to `cmd /c rmdir`, and
/// attempts to identify the locking process via handle64.exe when present.
fn remove_worktree_dir(path: &Path, name: &str) -> bool {
    let delays_ms = [100u64, 200, 400, 800, 1600];
    for delay in &delays_ms {
        if std::fs::remove_dir_all(path).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(*delay));
    }
    if std::fs::remove_dir_all(path).is_ok() {
        return true;
    }

    #[cfg(windows)]
    {
        if std::process::Command::new("cmd")
            .args(["/c", "rmdir", "/s", "/q", &path.to_string_lossy()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return true;
        }

        // `rmdir`/`remove_dir_all` both fail identically against a genuine
        // ACL denial, not just a transient in-use lock -- item #267's actual
        // failure mode: cargo's own `target/*/.fingerprint/*` files can end
        // up ACL-restricted (not merely open), which no amount of retrying
        // or an in-use-lock-only tool like `rmdir` can clear. `icacls /grant
        // ... /T` resets ownership access recursively before one final
        // delete attempt; a no-op (and thus never destructive) if the real
        // problem was actually an in-use lock the retries above already
        // would have cleared.
        if let Ok(user) = std::env::var("USERNAME") {
            let icacls_args: Vec<String> = vec![
                path.to_string_lossy().to_string(),
                "/grant".to_string(),
                format!("{user}:F"),
                "/T".to_string(),
                "/C".to_string(),
                "/Q".to_string(),
            ];
            let _ = std::process::Command::new("icacls")
                .args(&icacls_args)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
            if std::fs::remove_dir_all(path).is_ok() {
                return true;
            }
        }

        for handle_exe in &["handle64.exe", "handle.exe"] {
            if let Ok(output) = std::process::Command::new(handle_exe)
                .args(["-accepteula", "-nobanner", &path.to_string_lossy()])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .output()
            {
                if output.status.success() {
                    let out = String::from_utf8_lossy(&output.stdout);
                    for line in out.lines().take(5) {
                        eprintln!("worktree: locking process for '{}': {}", name, line);
                    }
                }
                break;
            }
        }
    }

    eprintln!(
        "worktree: failed to remove orphan '{}': Permission denied",
        name
    );
    false
}

/// Recursive directory size in bytes.
fn dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
        .sum()
}

#[cfg(test)]
#[path = "worktree_tests.rs"]
mod tests;
