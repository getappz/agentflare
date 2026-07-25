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

/// Adds `.worktrees/` to this repo's LOCAL, untracked ignore rules
/// (`.git/info/exclude`) rather than the tracked `.gitignore` — a claim
/// should never create a commit in the caller's repository (would sweep up
/// any unrelated staged files, and any pre-existing uncommitted `.gitignore`
/// edits, into a commit the agent didn't ask for).
pub fn ensure_worktrees_ignored(repo_root: &Path) {
    let Ok(common_dir) = run_git_in(repo_root, &["rev-parse", "--git-common-dir"]) else {
        return;
    };
    let exclude_path = repo_root.join(common_dir).join("info").join("exclude");
    if let Ok(existing) = std::fs::read_to_string(&exclude_path)
        && existing
            .lines()
            .any(|l| l.trim() == ".worktrees/" || l.trim() == ".worktrees")
    {
        return;
    }
    let mut content = String::new();
    if let Ok(existing) = std::fs::read_to_string(&exclude_path) {
        content = existing;
    }
    if !content.ends_with('\n') && !content.is_empty() {
        content.push('\n');
    }
    content.push_str(".worktrees/\n");
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
    let branch = format!("task/{}", item.sequence_id);
    let worktree_path = repo_root
        .join(".worktrees")
        .join("task")
        .join(item.sequence_id.to_string());
    if already_isolated_for(&branch, repo_root) {
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
        let _ = run_output_timeout(
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
        let start_point = match run_output_timeout(
            crate::shell::git_binary(),
            &["fetch", "origin", target_branch],
            repo_root,
            fetch_timeout_secs,
        ) {
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
                eprintln!(
                    "worktree: could not fetch '{target_branch}' from origin, branching off the local ref instead"
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
fn run_output_timeout(
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
    let branch = format!("task/{}", item.sequence_id);
    let worktree_path = repo_root
        .join(".worktrees")
        .join("task")
        .join(item.sequence_id.to_string());
    if !worktree_path.exists() {
        return None; // nothing was ever claimed into a worktree for this item
    }
    // Nothing to push (and nothing worth a PR) if the branch never
    // diverged from its target — e.g. `done` called with no commits made.
    match run_git_in(
        repo_root,
        &["rev-list", "--count", &format!("{target_branch}..{branch}")],
    ) {
        Ok(count) if count != "0" => {}
        _ => return None,
    }
    // Content-diff guard: even when the branch has new commits, its
    // *content* may already be on the target (squash-merge). Compares
    // target→branch tree (two-dot, not three-dot: we want whether the
    // two tips are identical, not whether branch differs from merge-base).
    if run_git_in_ok(
        repo_root,
        &["diff", "--quiet", &format!("{target_branch}..{branch}")],
    ) {
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
        // Only consider as orphan when the gitdir pointer is broken
        if !has_broken_gitdir {
            continue;
        }
        let sequence_id = dir_name.parse::<i64>().ok();
        let size_bytes = dir_size(path);
        orphans.push(OrphanWorktree {
            name: dir_name,
            path: path.to_path_buf(),
            sequence_id,
            size_bytes,
            has_broken_gitdir,
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
    // Prune git's worktree metadata after removal
    if !deleted.is_empty() {
        let _ = crate::shell::run_in(repo_root, &["worktree", "prune"]);
    }
    deleted
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
mod tests {
    use super::*;
    use crate::shell::test_support::{Repo, init_repo_with_branch};
    use tempfile::TempDir;

    fn init_repo() -> Repo {
        init_repo_with_branch("master")
    }

    #[test]
    fn remove_worktree_dir_succeeds_immediately_when_unlocked() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("wt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("file.txt"), b"hello").unwrap();
        assert!(remove_worktree_dir(&dir, "test"));
        assert!(!dir.exists());
    }

    #[test]
    fn remove_worktree_dir_retries_past_a_transient_lock() {
        // On Windows, an open file handle blocks remove_dir_all with
        // Permission denied until released -- exactly item #302's failure
        // mode (rust-analyzer holding a worktree file open). On Unix this
        // doesn't block deletion at all, so the test still passes there,
        // just without exercising the retry path meaningfully.
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().join("wt");
        std::fs::create_dir_all(&dir).unwrap();
        let locked_file = dir.join("locked.txt");
        std::fs::write(&locked_file, b"hello").unwrap();

        let file = std::fs::File::open(&locked_file).unwrap();
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            drop(file);
        });

        assert!(
            remove_worktree_dir(&dir, "test"),
            "must succeed once the lock clears, within the retry budget"
        );
        assert!(!dir.exists());
        releaser.join().unwrap();
    }

    fn test_item(sequence_id: i64) -> Item {
        Item {
            id: "test-id".into(),
            project_id: "proj".into(),
            state_id: "state".into(),
            name: "test".into(),
            description: String::new(),
            priority: "none".into(),
            parent_id: None,
            assignee_agent: None,
            sequence_id,
            sort_order: 0.0,
            started_at: None,
            completed_at: None,
            archived_at: None,
            external_source: None,
            external_id: None,
            metadata: "{}".into(),
            created_at: 0,
            updated_at: 0,
            deleted_at: None,
        }
    }

    #[test]
    fn ensure_worktrees_ignored_is_noop_when_already_ignored() {
        let repo = init_repo();
        let exclude_path = repo.path.join(".git").join("info").join("exclude");
        std::fs::create_dir_all(exclude_path.parent().unwrap()).unwrap();
        std::fs::write(&exclude_path, ".worktrees/\n").unwrap();
        let before = std::fs::read_to_string(&exclude_path).unwrap();
        ensure_worktrees_ignored(&repo.path);
        let after = std::fs::read_to_string(&exclude_path).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn ensure_worktrees_ignored_adds_to_local_exclude_without_committing() {
        let repo = init_repo();
        ensure_worktrees_ignored(&repo.path);
        let exclude_path = repo.path.join(".git").join("info").join("exclude");
        let content = std::fs::read_to_string(&exclude_path).unwrap();
        assert!(content.contains(".worktrees/"));
        // Must never touch the tracked .gitignore or create a commit.
        assert!(!repo.path.join(".gitignore").exists());
        let log = run_git_in(&repo.path, &["log", "--oneline"]).unwrap();
        assert_eq!(
            log.lines().count(),
            1,
            "no new commit should have been made"
        );
    }

    #[test]
    fn already_isolated_for_false_in_regular_repo() {
        let repo = init_repo();
        assert!(!already_isolated_for("task/1", &repo.path));
    }

    #[test]
    fn isolate_worktree_target_dir_writes_relative_target_dir() {
        let tmp = TempDir::new().unwrap();
        let wt = tmp.path().join(".worktrees").join("task").join("1");
        std::fs::create_dir_all(&wt).unwrap();
        isolate_worktree_target_dir(&wt);
        let config = wt.join(".cargo").join("config.toml");
        assert!(config.exists(), "expected .cargo/config.toml in worktree");
        let content = std::fs::read_to_string(&config).unwrap();
        assert!(
            content.contains("target-dir = \"target\""),
            "must set a relative, per-checkout target dir, got: {content}"
        );
        assert!(
            !content.contains("target-dir = \"/")
                && !content.contains("target-dir = \"~")
                && !content.contains("CARGO_TARGET_DIR ="),
            "must not set an absolute/shared target dir"
        );
    }

    #[test]
    fn isolate_worktree_target_dir_does_not_clobber_existing_config() {
        let tmp = TempDir::new().unwrap();
        let wt = tmp.path().join(".worktrees").join("task").join("1");
        let cargo_dir = wt.join(".cargo");
        std::fs::create_dir_all(&cargo_dir).unwrap();
        let config = cargo_dir.join("config.toml");
        std::fs::write(
            &config,
            "[build]\ntarget-dir = \"/some/intentional/path\"\n",
        )
        .unwrap();
        isolate_worktree_target_dir(&wt);
        let content = std::fs::read_to_string(&config).unwrap();
        assert!(
            content.contains("/some/intentional/path"),
            "existing worktree-local config must be preserved"
        );
    }

    #[test]
    fn isolate_worktree_target_dir_wires_sccache_when_available() {
        let tmp = TempDir::new().unwrap();
        let wt = tmp.path().join(".worktrees").join("task").join("1");
        std::fs::create_dir_all(&wt).unwrap();
        isolate_worktree_target_dir(&wt);
        let config = wt.join(".cargo").join("config.toml");
        let content = std::fs::read_to_string(&config).unwrap();
        if sccache_available() {
            assert!(
                content.contains("rustc-wrapper = \"sccache\""),
                "expected sccache wired up as rustc-wrapper, got: {content}"
            );
            let escaped = wt
                .to_string_lossy()
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            let basedir_line = format!("SCCACHE_BASEDIRS = \"{escaped}\"");
            assert!(
                content.contains(&basedir_line),
                "expected SCCACHE_BASEDIRS to strip this worktree's own path, got: {content}"
            );
        } else {
            assert!(
                !content.contains("rustc-wrapper") && !content.contains("SCCACHE_BASEDIRS"),
                "must not reference sccache when it isn't on PATH, got: {content}"
            );
        }
    }

    #[test]
    fn warn_if_ambient_target_dir_warns_when_set() {
        // Just asserts the function runs without panicking whether or not the
        // var is set; the warning is an ephemeral eprintln, not assertable here.
        unsafe {
            std::env::set_var("CARGO_TARGET_DIR", "/tmp/shared");
        }
        warn_if_ambient_target_dir();
        unsafe {
            std::env::remove_var("CARGO_TARGET_DIR");
        }
        warn_if_ambient_target_dir();
    }

    #[test]
    fn already_isolated_for_true_inside_the_worktree_it_created() {
        let repo = init_repo();
        let item = test_item(1);
        let target = resolve_default_branch(&repo.path);
        let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();
        assert!(already_isolated_for("task/1", &worktree_path));
    }

    #[test]
    fn create_worktree_creates_worktree_and_branch() {
        let repo = init_repo();
        let worktree_path = repo.path.join(".worktrees").join("task").join("1");
        let item = test_item(1);
        let target = resolve_default_branch(&repo.path);
        let result = create_worktree(&item, &repo.path, &target, None);
        assert!(result.is_ok());
        assert!(worktree_path.exists());
    }

    #[test]
    fn create_worktree_reuses_a_branch_that_exists_with_no_owning_worktree() {
        // The task/29 collision: a plain local branch survives (e.g. from a
        // prior session) with no worktree ever created for it. `-b` alone
        // would fail here ("a branch named 'task/1' already exists") --
        // create_worktree must detect this and check out the existing
        // branch instead of trying to recreate it.
        let repo = init_repo();
        run_git_in(&repo.path, &["branch", "task/1"]).unwrap();
        let worktree_path = repo.path.join(".worktrees").join("task").join("1");
        let item = test_item(1);
        let target = resolve_default_branch(&repo.path);
        let result = create_worktree(&item, &repo.path, &target, None);
        assert!(result.is_ok(), "{:?}", result.err());
        assert!(worktree_path.exists());
        let checked_out = run_git_in(&worktree_path, &["branch", "--show-current"]).unwrap();
        assert_eq!(checked_out, "task/1");
    }

    #[test]
    fn create_worktree_soft_fails_on_bad_git() {
        let tmp = TempDir::new().unwrap();
        let bad_root = tmp.path().join("not-a-repo");
        std::fs::create_dir_all(&bad_root).unwrap();
        let item = test_item(1);
        let result = create_worktree(&item, &bad_root, "master", None);
        let err = result.expect_err("not a git repo, worktree add must fail");
        assert!(
            err.contains(&item.id),
            "error must name the item it failed for: {err}"
        );
        assert!(
            err.len() > format!("worktree: creation skipped for item {}: ", item.id).len(),
            "error must carry the underlying git failure detail, not just the prefix: {err}"
        );
    }

    #[test]
    fn create_worktree_fetches_target_branch_and_includes_remote_only_commits() {
        // "remote" — plays the role of `origin`.
        let remote = init_repo();
        // "local" — a clone that will go stale the moment `remote` gets a
        // new commit; this is what `create_worktree` actually operates on.
        let local_container = TempDir::new().unwrap();
        let local_path = local_container.path().join("local");
        run_git_in(
            local_container.path(),
            &[
                "clone",
                remote.path.to_str().unwrap(),
                local_path.to_str().unwrap(),
            ],
        )
        .unwrap();
        run_git_in(&local_path, &["config", "user.email", "test@test.com"]).unwrap();
        run_git_in(&local_path, &["config", "user.name", "Test"]).unwrap();

        // Lands on the remote *after* the clone — local's own `master` and
        // `origin/master` are both stale relative to this.
        run_git_in(
            &remote.path,
            &["commit", "--allow-empty", "-m", "remote-only commit"],
        )
        .unwrap();
        let remote_head = run_git_in(&remote.path, &["rev-parse", "HEAD"]).unwrap();

        let item = test_item(1);
        let worktree_path = create_worktree(&item, &local_path, "master", None).unwrap();
        let worktree_head = run_git_in(&worktree_path, &["rev-parse", "HEAD"]).unwrap();

        assert_eq!(
            worktree_head, remote_head,
            "worktree must be based on the freshly-fetched remote commit, not the stale local ref"
        );
    }

    #[test]
    fn push_branch_returns_none_when_no_worktree_exists() {
        let repo = init_repo();
        let item = test_item(1);
        assert!(push_branch(&item, &repo.path, "master", None).is_none());
    }

    #[test]
    fn push_branch_returns_none_when_branch_has_no_new_commits() {
        let repo = init_repo();
        let item = test_item(1);
        let target = resolve_default_branch(&repo.path);
        create_worktree(&item, &repo.path, &target, None).unwrap();
        // No commits were made in the worktree — nothing to push, so this
        // must return early without attempting a real `git push` (which
        // would fail anyway: no remote configured here).
        assert!(push_branch(&item, &repo.path, &target, None).is_none());
    }

    #[test]
    fn push_branch_returns_none_when_branch_content_already_merged() {
        let repo = init_repo();
        let item = test_item(1);
        let target = resolve_default_branch(&repo.path);
        let worktree_path = create_worktree(&item, &repo.path, &target, None).unwrap();
        let test_file = worktree_path.join("test.txt");
        std::fs::write(&test_file, b"hello").unwrap();
        run_git_in(&worktree_path, &["add", "test.txt"]).unwrap();
        run_git_in(&worktree_path, &["commit", "-m", "worktree change"]).unwrap();
        // task/1 now has a commit master doesn't. Squash-merge: cherry-pick
        // the *diff* onto master so content matches but ancestry doesn't.
        run_git_in(&repo.path, &["cherry-pick", "-n", "task/1"]).unwrap();
        run_git_in(&repo.path, &["commit", "-m", "squash-merge"]).unwrap();
        // Content-diff guard should catch this even though commit-count
        // guard passes.
        assert!(push_branch(&item, &repo.path, &target, None).is_none());
    }

    #[test]
    fn run_output_timeout_kills_the_child_not_just_abandons_it() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = tmp.path().join("marker");
        // A command that, left alone, outlives our 1s timeout and then
        // writes `marker`. If the timeout only abandoned the child (the bug
        // this replaced) rather than killing it, the marker would still
        // show up once the sleep finishes on its own.
        #[cfg(unix)]
        let (program, owned_args): (&str, Vec<String>) = (
            "sh",
            vec![
                "-c".into(),
                format!("sleep 3 && touch {}", marker.display()),
            ],
        );
        #[cfg(windows)]
        let (program, owned_args): (&str, Vec<String>) = (
            "cmd",
            vec![
                "/C".into(),
                format!(
                    "ping 127.0.0.1 -n 4 >NUL & echo done > {}",
                    marker.display()
                ),
            ],
        );
        let args: Vec<&str> = owned_args.iter().map(String::as_str).collect();

        let result = run_output_timeout(program, &args, tmp.path(), 1);
        assert!(
            matches!(&result, Err(e) if e.contains("timed out")),
            "{result:?}"
        );

        // Give the command's natural (un-killed) duration time to elapse
        // before checking the marker never showed up.
        std::thread::sleep(Duration::from_secs(4));
        assert!(
            !marker.exists(),
            "child kept running after the timeout — it was abandoned, not killed"
        );
    }
}
