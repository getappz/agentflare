//! Bubblewrap-based sandboxing for job commands on native Linux and WSL2.
//!
//! Flag set and ordering are adapted from openai/codex's `linux-sandbox`
//! crate (`bwrap.rs`): read-only root by default (`--ro-bind / /`), an
//! explicit writable bind for the job's cwd, `.git` re-protected read-only
//! even though it sits inside that writable root (unless the caller passes
//! `git_writable = true` -- see [`wrap`]'s doc comment), and `--chdir` into
//! the cwd rather than relying on bubblewrap inheriting a possibly-symlinked
//! logical cwd. Unlike codex's crate this has no split filesystem policy,
//! glob-based path masking, or seccomp network filter -- this job runner
//! operates on worktrees it already controls, not arbitrary third-party
//! workspaces, so that machinery isn't needed here.
mod bwrap_install;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// $HOME subdirectories commonly needed by build/package tooling (cargo,
/// rustup, npm, pip caches) that would otherwise sit outside the job's cwd
/// and fail under the read-only root. Bound read-only with `--ro-bind-try`
/// (missing dirs silently skipped) -- writable would let a sandboxed job
/// persist changes into the host's caches (e.g. a malicious `~/.cargo/bin`
/// entry or poisoned npm cache) that run unsandboxed on a future invocation,
/// defeating the containment this sandbox exists to provide.
const HOME_CACHE_DIRS: &[&str] = &[".cargo", ".rustup", ".cache", ".npm"];

/// opencode's own data dir, relative to `$HOME` -- unlike claude-code/codex/
/// gemini's headless modes, `opencode run` unconditionally writes into this
/// directory on every invocation (an append-mode log file, plus a SQLite
/// session DB it checkpoints), which crashes outright under the read-only
/// root (item #106: `FileSystem.open` on `opencode.log`, then a WAL
/// checkpoint failure once that's worked around). It also holds
/// `auth.json`, so it can't just go on `HOME_CACHE_DIRS` read-only -- opencode
/// needs to both read existing credentials/config *and* write. Mounted via
/// `--overlay-src`+`--tmp-overlay` in `build_bwrap_args_with_home`: reads
/// see the real directory, writes land in an invisible tmpfs that's
/// discarded when the sandboxed process exits, so nothing persists back to
/// the host -- same "no writable state survives past this one job" guarantee
/// `HOME_CACHE_DIRS` argues for, just via overlay instead of read-only-try
/// since this one dir needs both.
const OPENCODE_DATA_DIR_RELATIVE: &str = ".local/share/opencode";

/// claude-code's own data dir, relative to `$HOME`. It holds
/// `.credentials.json`, the file claude-code's OAuth refresh machinery both
/// reads (existing access/refresh tokens) and writes (persisting the rotated
/// token pair after a successful server-side refresh). The write side is the
/// part the sandbox breaks: claude-code's refresh flow first creates
/// `.oauth_refresh.lock` (inside this dir) and later rewrites the credentials
/// file via a temp-file+rename, and both fail with EROFS when the dir falls
/// through to the sandbox's read-only root -- the refresh never completes and
/// the job dies with "Failed to authenticate. API Error: 401 OAuth access
/// token has expired" against the stale token (item #127, confirmed live:
/// every sandboxed job hit it while the same token refreshed fine
/// interactively). Mounted via `--overlay-src`+`--tmp-overlay` in
/// `build_bwrap_args_with_home`, exactly like `OPENCODE_DATA_DIR_RELATIVE`:
/// reads see the real dir, writes land in an invisible tmpfs that's discarded
/// when the sandboxed process exits, so the in-job refresh can complete
/// without any refreshed token persisting back to the host's real
/// `~/.claude` -- same "no writable state survives past this one job"
/// guarantee `HOME_CACHE_DIRS` argues for.
const CLAUDE_DATA_DIR_RELATIVE: &str = ".claude";

/// cursor-agent's own config/state dir, relative to `$HOME`. It holds
/// `mcp.json`/`hooks.json` (read) plus a per-project tracking directory
/// cursor-agent creates on demand under `.cursor/projects/<slug>` for
/// whatever cwd it's launched against -- item #130, confirmed live: with
/// `.cursor` left off the sandbox entirely (falling through to the
/// `--ro-bind / /` read-only root, same as any other unlisted `$HOME`
/// subdir), that `mkdir` failed and every cursor-agent job died immediately
/// with "cursor exited non-zero -- ENOENT ... mkdir
/// '.../.cursor/projects/<slug>'" before doing any real work. Mounted via
/// `--overlay-src`+`--tmp-overlay` in `build_bwrap_args_with_home`, exactly
/// like `CLAUDE_DATA_DIR_RELATIVE`/`OPENCODE_DATA_DIR_RELATIVE`: reads see
/// the real dir (existing `mcp.json`/`hooks.json`/auth), writes -- including
/// that per-project tracking dir -- land in an invisible tmpfs discarded
/// when the sandboxed process exits, so nothing persists back to the host.
const CURSOR_DATA_DIR_RELATIVE: &str = ".cursor";

/// The agentflare MCP server's own state dir, relative to `$HOME` -- holds
/// `agentflare.db`, the sqlite store every `item`/`comment`/`vent` MCP call
/// writes through. Unlike `HOME_CACHE_DIRS`, this one must be writable: a
/// dispatched job's whole job-completion signal (`item action=done`,
/// `comment action=create`, even `vent` for reporting a sandbox problem like
/// this one) is an MCP call into this same DB, so read-only here means the
/// agent finishes real work with no way to report it (item #120 -- confirmed
/// live via `EROFS` on `touch ~/.agentflare/probe` and two dispatched jobs
/// stuck showing not-done despite merge-ready PRs). Bound with `--bind-try`
/// (writable, silently skipped if missing) rather than `--bind`, matching
/// the "missing dir is fine, wrong dir is not" fallback the other optional
/// home binds use.
const AGENTFLARE_DATA_DIR_RELATIVE: &str = ".agentflare";

/// Whether `command` (the resolved binary about to be run, e.g.
/// `/home/user/.opencode/bin/opencode`) is opencode -- matched on the final
/// path component so it doesn't care whether the caller passed a bare name
/// or a full resolved path.
fn is_opencode(command: &str) -> bool {
    Path::new(command).file_name() == Some(std::ffi::OsStr::new("opencode"))
}

/// Whether `command` (the resolved binary about to be run) is claude-code --
/// matched on the final path component `claude` (the CLI's binary name, found
/// via `find_binary(["claude"])`), so it works for both a bare name and a
/// full resolved path, the same way `is_opencode` does.
fn is_claude_code(command: &str) -> bool {
    Path::new(command).file_name() == Some(std::ffi::OsStr::new("claude"))
}

/// Whether `command` (the resolved binary about to be run) is cursor-agent --
/// matched on the final path component `cursor-agent` (the binary name from
/// `agent-registry`'s `binary_names: &["cursor-agent"]`), the same way
/// `is_opencode`/`is_claude_code` do.
fn is_cursor(command: &str) -> bool {
    Path::new(command).file_name() == Some(std::ffi::OsStr::new("cursor-agent"))
}

/// `git_writable` controls whether `cwd/.git` is re-protected read-only
/// (the default, `false` -- appropriate for an arbitrary job command that
/// has no business rewriting git history) or left writable under the same
/// `--bind` as the rest of `cwd` (`true` -- required by the headless
/// coding-agent CLI itself, whose entire job is `git add`/`git commit`/
/// `git push`; see `agent_launch::run_headless`'s call site).
pub(super) fn wrap(
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
    git_writable: bool,
) -> Option<(String, Vec<String>)> {
    let bwrap = find_or_install_bwrap()?;
    // Resolved once up front (not inside `build_bwrap_args`) so an
    // unresolvable cwd falls back to running the job unsandboxed, the same
    // as a missing `bwrap`/`mise` -- rather than binding a plausible-looking
    // but wrong directory into the sandbox. A `None` cwd is left as `None`
    // (nothing to resolve); only a `Some(path)` that fails to resolve bails
    // the whole function out via `?`.
    let cwd = match cwd {
        Some(path) => Some(absolute_path(path)?),
        None => None,
    };
    Some((
        path_to_string(&bwrap),
        build_bwrap_args(cwd.as_deref(), command, args, git_writable),
    ))
}

fn build_bwrap_args(
    cwd: Option<&Path>,
    command: &str,
    args: &[String],
    git_writable: bool,
) -> Vec<String> {
    build_bwrap_args_with_home(
        cwd,
        command,
        args,
        std::env::var_os("HOME").as_deref(),
        git_writable,
    )
}

fn build_bwrap_args_with_home(
    cwd: Option<&Path>,
    command: &str,
    args: &[String],
    home: Option<&std::ffi::OsStr>,
    git_writable: bool,
) -> Vec<String> {
    let mut bwrap_args = vec![
        "--new-session".to_string(),
        "--die-with-parent".to_string(),
        "--unshare-user".to_string(),
        "--unshare-pid".to_string(),
        "--ro-bind".to_string(),
        "/".to_string(),
        "/".to_string(),
        "--dev".to_string(),
        "/dev".to_string(),
        "--proc".to_string(),
        "/proc".to_string(),
        "--bind-try".to_string(),
        "/dev/shm".to_string(),
        "/dev/shm".to_string(),
        // A private, empty tmpfs -- not the host's real /tmp, which would
        // otherwise be fully read-write to the job and shared with every
        // other process (including other concurrently-sandboxed jobs) on
        // the box.
        "--tmpfs".to_string(),
        "/tmp".to_string(),
    ];

    if let Some(cwd) = cwd {
        let cwd_str = path_to_string(cwd);
        bwrap_args.push("--bind".to_string());
        bwrap_args.push(cwd_str.clone());
        bwrap_args.push(cwd_str.clone());

        let git_dir = cwd.join(".git");
        match std::fs::symlink_metadata(&git_dir) {
            Ok(meta) if meta.is_file() => {
                // `cwd/.git` is a `git worktree` gitfile (`gitdir: <path>`),
                // not a real metadata directory -- the actual per-worktree
                // admin files (HEAD, index, index.lock, logs/HEAD) and the
                // shared object/ref store `git commit` needs to write into
                // live under the main repo's `.git`, resolved via the admin
                // dir's own `commondir` file. That resolved directory sits
                // outside `cwd` entirely, so it needs its own explicit bind
                // regardless of `git_writable` -- unlike the plain-repo case
                // below, it isn't already covered by `cwd`'s `--bind`.
                if let Some(common_dir) = resolve_worktree_common_dir(cwd, &git_dir) {
                    let common_str = path_to_string(&common_dir);
                    bwrap_args.push(if git_writable { "--bind" } else { "--ro-bind" }.to_string());
                    bwrap_args.push(common_str.clone());
                    bwrap_args.push(common_str);
                }
            }
            Ok(meta) if meta.is_dir() && !git_writable => {
                let git_str = path_to_string(&git_dir);
                bwrap_args.push("--ro-bind".to_string());
                bwrap_args.push(git_str.clone());
                bwrap_args.push(git_str);
            }
            _ => {}
        }

        bwrap_args.push("--chdir".to_string());
        bwrap_args.push(cwd_str);
    }

    if let Some(home) = home {
        for cache in HOME_CACHE_DIRS {
            let path = Path::new(home).join(cache);
            if path.exists() {
                let path_str = path_to_string(&path);
                bwrap_args.push("--ro-bind-try".to_string());
                bwrap_args.push(path_str.clone());
                bwrap_args.push(path_str);
            }
        }

        let agentflare_dir = Path::new(home).join(AGENTFLARE_DATA_DIR_RELATIVE);
        if agentflare_dir.exists() {
            let agentflare_str = path_to_string(&agentflare_dir);
            bwrap_args.push("--bind-try".to_string());
            bwrap_args.push(agentflare_str.clone());
            bwrap_args.push(agentflare_str);
        }

        if is_opencode(command) {
            let opencode_dir = Path::new(home).join(OPENCODE_DATA_DIR_RELATIVE);
            let opencode_str = path_to_string(&opencode_dir);
            if opencode_dir.exists() {
                // `--overlay-src` requires its source to already exist;
                // reads pass through to it, writes go to the implicit tmpfs
                // `--tmp-overlay` adds on top -- never touching the host.
                bwrap_args.push("--overlay-src".to_string());
                bwrap_args.push(opencode_str.clone());
                bwrap_args.push("--tmp-overlay".to_string());
                bwrap_args.push(opencode_str);
            } else {
                // Never run before under this `$HOME` -- nothing to read,
                // so a plain writable tmpfs (no read-only source needed)
                // covers the same "opencode can create+write its own data
                // dir" case.
                bwrap_args.push("--tmpfs".to_string());
                bwrap_args.push(opencode_str);
            }
        }

        if is_claude_code(command) {
            let claude_dir = Path::new(home).join(CLAUDE_DATA_DIR_RELATIVE);
            let claude_str = path_to_string(&claude_dir);
            if claude_dir.exists() {
                // Same overlay semantics as the opencode dir above: reads
                // pass through to the host's `~/.claude` (existing
                // `.credentials.json`), writes -- the `.oauth_refresh.lock`
                // and rotated-token persist claude-code's refresh flow needs
                // -- go to the implicit tmpfs `--tmp-overlay` adds on top,
                // discarded on exit, never touching the host (item #127).
                bwrap_args.push("--overlay-src".to_string());
                bwrap_args.push(claude_str.clone());
                bwrap_args.push("--tmp-overlay".to_string());
                bwrap_args.push(claude_str);
            } else {
                // Never run before under this `$HOME` -- no existing
                // credentials to read, but claude-code still needs a
                // writable dir to create `.credentials.json` (it re-auths
                // into the overlay, discarded after this one job).
                bwrap_args.push("--tmpfs".to_string());
                bwrap_args.push(claude_str);
            }
        }

        if is_cursor(command) {
            let cursor_dir = Path::new(home).join(CURSOR_DATA_DIR_RELATIVE);
            let cursor_str = path_to_string(&cursor_dir);
            if cursor_dir.exists() {
                // Same overlay semantics as claude/opencode above: reads
                // pass through to the host's `~/.cursor` (existing
                // `mcp.json`/`hooks.json`/auth), writes -- including the
                // per-project tracking dir cursor-agent creates on demand --
                // go to the implicit tmpfs `--tmp-overlay` adds on top,
                // discarded on exit, never touching the host (item #130).
                bwrap_args.push("--overlay-src".to_string());
                bwrap_args.push(cursor_str.clone());
                bwrap_args.push("--tmp-overlay".to_string());
                bwrap_args.push(cursor_str);
            } else {
                // Never run before under this `$HOME` -- nothing to read,
                // but cursor-agent still needs a writable dir to create its
                // project-tracking subdirectory (re-created into the
                // overlay, discarded after this one job).
                bwrap_args.push("--tmpfs".to_string());
                bwrap_args.push(cursor_str);
            }
        }
    }

    bwrap_args.push("--".to_string());
    bwrap_args.push(command.to_string());
    bwrap_args.extend(args.iter().cloned());

    bwrap_args
}

/// Resolves a `git worktree` checkout's `cwd/.git` gitfile
/// (`gitdir: <path-to-worktree-admin-dir>`) to the main repo's real `.git`
/// directory, following the admin dir's own `commondir` file -- git's
/// documented mechanism for a worktree to find the common dir shared across
/// all worktrees (typically `../..` from the admin dir, but not assumed).
/// Returns `None` on any read/parse/resolve failure, so the caller skips the
/// bind entirely rather than mounting a wrong or partial path into the
/// sandbox.
fn resolve_worktree_common_dir(cwd: &Path, git_file: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(git_file).ok()?;
    let gitdir_line = contents
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?;
    let admin_dir = resolve_against(cwd, Path::new(gitdir_line.trim()));

    let commondir_contents = std::fs::read_to_string(admin_dir.join("commondir")).ok()?;
    let common_dir = resolve_against(&admin_dir, Path::new(commondir_contents.trim()));

    std::fs::canonicalize(common_dir).ok()
}

/// Joins `path` onto `base` if relative, leaving an already-absolute `path`
/// untouched -- both the `gitdir:` line and `commondir` file are written
/// absolute by git in practice, but the format doesn't guarantee it.
fn resolve_against(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// Resolves `path` to a canonical absolute path -- symlinks and `.`/`..`
/// components resolved by the OS (`realpath` semantics), not stripped
/// lexically. bwrap's own `--bind`/`--chdir` source paths are taken
/// literally, unlike `std::process::Command::current_dir`, which resolves a
/// relative path against the parent's cwd before handing it to the child.
/// Lexically stripping `..` instead of asking the OS would be wrong the
/// moment a symlink sits in the path (`link/..` is the parent of whatever
/// `link` points at, not the lexical parent of `link` itself). Returns
/// `None` if the path doesn't exist or can't be resolved, so the caller
/// falls back to unsandboxed rather than bind a wrong directory.
fn absolute_path(path: &Path) -> Option<PathBuf> {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    std::fs::canonicalize(joined).ok()
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Finds `bwrap` on `PATH`, installing it via `bwrap_install` on first miss,
/// and caches the result for the life of the process so a box without
/// bubblewrap (or without `mise`) doesn't retry the install on every job.
/// Logs the outcome either way -- a job silently running unsandboxed because
/// bwrap was missing should never look identical to a sandboxed one.
fn find_or_install_bwrap() -> Option<PathBuf> {
    static BWRAP: OnceLock<Option<PathBuf>> = OnceLock::new();
    BWRAP
        .get_or_init(|| {
            if let Some(path) = which_bwrap() {
                return Some(path);
            }
            match bwrap_install::install() {
                Some(path) => {
                    eprintln!(
                        "agentflare-jobs: installed bwrap via mise at {}",
                        path.display()
                    );
                    Some(path)
                }
                None => {
                    eprintln!(
                        "agentflare-jobs: bwrap not found and could not be installed \
                         (mise missing or install failed) -- running this job unsandboxed"
                    );
                    None
                }
            }
        })
        .clone()
}

/// Minimal `PATH` scan for `bwrap`. This crate has no `which` dependency, and
/// `Command::new("bwrap")` alone won't tell us up front whether it's missing
/// -- checking here lets a job fall back to running unsandboxed (rather than
/// failing to spawn at all) when a box hasn't installed bubblewrap yet.
fn which_bwrap() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    find_on_path(&path, "bwrap")
}

/// Pure PATH search, factored out so tests can exercise it without mutating
/// the real process-wide `PATH` env var -- `std::env::set_var` affects every
/// thread in the test binary, and cargo's test harness runs tests in
/// parallel by default, so an env-mutating test here would race with any
/// other test that spawns a child process expecting `PATH` to be intact.
fn find_on_path(path: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

/// True if `path` is a regular file with at least one executable bit set.
/// Matching on name + `is_file()` alone would happily "find" a stray
/// non-executable file and hand it to `Command::new`, which then fails to
/// spawn at all -- defeating the fallback-to-unsandboxed path this lookup
/// exists for.
pub(super) fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_bwrap_falls_back_to_none() {
        assert!(find_on_path(std::ffi::OsStr::new(""), "bwrap").is_none());
    }

    #[test]
    fn non_executable_candidate_on_path_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("bwrap"), b"").unwrap();
        let path_var = std::ffi::OsString::from(dir.path());
        assert!(find_on_path(&path_var, "bwrap").is_none());
    }

    fn bind_arg(args: &[String]) -> (&str, &str) {
        let idx = args
            .iter()
            .position(|a| a == "--bind")
            .expect("--bind present");
        (&args[idx + 1], &args[idx + 2])
    }

    #[test]
    fn resolved_cwd_is_placed_into_bind_and_chdir_args() {
        let dir = tempfile::tempdir().unwrap();
        let cwd = std::fs::canonicalize(dir.path()).unwrap();
        let args = build_bwrap_args_with_home(Some(cwd.as_path()), "true", &[], None, false);
        let expected = path_to_string(&cwd);
        let (src, dest) = bind_arg(&args);
        assert_eq!(src, expected);
        assert_eq!(dest, expected);
        let chdir_idx = args
            .iter()
            .position(|a| a == "--chdir")
            .expect("--chdir present");
        assert_eq!(args[chdir_idx + 1], expected);
    }

    #[test]
    fn git_dir_is_re_protected_read_only_by_default() {
        // Item #88: a sandboxed job command (e.g. a build/test/lint job via
        // `Supervisor::spawn`) has no business rewriting git history, so
        // `.git` stays read-only even though it sits inside the writable
        // cwd bind -- the documented default this module's doc comment
        // describes, now actually under test.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let cwd = std::fs::canonicalize(dir.path()).unwrap();
        let args = build_bwrap_args_with_home(Some(cwd.as_path()), "true", &[], None, false);
        let git_str = path_to_string(&cwd.join(".git"));
        let idx = args
            .iter()
            .position(|a| a == &git_str)
            .expect(".git path present in args");
        assert_eq!(args[idx - 1], "--ro-bind");
    }

    #[test]
    fn git_dir_stays_writable_when_the_caller_needs_to_commit() {
        // Item #88: `run_headless` spawns the headless coding-agent CLI
        // itself, whose entire job is `git add`/`git commit`/`git push` --
        // re-protecting `.git` read-only for that specific caller made every
        // headless work-item dispatch unable to ever commit its own staged
        // changes (bwrap denies the write with "Read-only file system",
        // which `commit_uncommitted` in flare-git-core then swallows
        // silently, so `item_done` reports success with nothing committed).
        // `git_writable = true` must leave `.git` under the plain writable
        // `--bind` for cwd instead of adding a second read-only bind on top.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let cwd = std::fs::canonicalize(dir.path()).unwrap();
        let args = build_bwrap_args_with_home(Some(cwd.as_path()), "true", &[], None, true);
        let git_str = path_to_string(&cwd.join(".git"));
        assert!(
            !args.iter().any(|a| a == &git_str),
            ".git must not appear as a separate bind entry when git_writable is true: {args:?}"
        );
    }

    fn write_worktree_gitfile(cwd: &Path, admin_dir: &Path) {
        std::fs::write(
            cwd.join(".git"),
            format!("gitdir: {}\n", admin_dir.display()),
        )
        .unwrap();
    }

    #[test]
    fn worktree_gitfile_resolves_main_git_dir_and_binds_it_read_write() {
        // Item #90: a `git worktree` checkout's `cwd/.git` is a gitfile, not
        // a real metadata directory -- the actual admin files (HEAD, index,
        // index.lock, logs) and the shared object/ref store `git commit`
        // needs to write into live under the main repo's `.git`, reached via
        // the admin dir's own `commondir` file. That resolved directory sits
        // outside `cwd` entirely, so with `git_writable = true` it needs its
        // own explicit `--bind` (not `--ro-bind`) -- otherwise it falls back
        // to the sandbox's default read-only root and `index.lock` creation
        // fails with "Read-only file system", exactly as observed live.
        let main_repo = tempfile::tempdir().unwrap();
        let main_git_dir = std::fs::canonicalize(main_repo.path())
            .unwrap()
            .join(".git");
        let admin_dir = main_git_dir.join("worktrees").join("task-90");
        std::fs::create_dir_all(&admin_dir).unwrap();
        std::fs::write(admin_dir.join("commondir"), "../..\n").unwrap();

        let worktree = tempfile::tempdir().unwrap();
        let cwd = std::fs::canonicalize(worktree.path()).unwrap();
        write_worktree_gitfile(&cwd, &admin_dir);

        let args = build_bwrap_args_with_home(Some(cwd.as_path()), "true", &[], None, true);

        let expected = path_to_string(&main_git_dir);
        let idx = args
            .iter()
            .position(|a| a == &expected)
            .expect("main repo .git dir bound");
        assert_eq!(args[idx - 1], "--bind");
    }

    #[test]
    fn plain_repo_git_dir_is_still_bound_read_only() {
        // Regression guard: a plain (non-worktree) repo's `.git` is a real
        // directory sitting inside `cwd`, so it must keep going through the
        // unchanged directory branch above, not the new gitfile-resolution
        // path.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let cwd = std::fs::canonicalize(dir.path()).unwrap();

        let args = build_bwrap_args_with_home(Some(cwd.as_path()), "true", &[], None, false);

        let expected = path_to_string(&cwd.join(".git"));
        let idx = args
            .iter()
            .position(|a| a == &expected)
            .expect(".git dir bound");
        assert_eq!(args[idx - 1], "--ro-bind");
    }

    #[test]
    fn absolute_path_resolves_relative_dot_against_current_dir() {
        let expected = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
        assert_eq!(absolute_path(Path::new(".")).unwrap(), expected);
    }

    #[test]
    fn absolute_path_canonicalizes_an_already_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(absolute_path(dir.path()).unwrap(), expected);
    }

    #[test]
    fn absolute_path_missing_directory_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        assert!(absolute_path(&dir.path().join("does-not-exist")).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn absolute_path_resolves_a_symlinked_component_via_its_real_parent() {
        // Lexically stripping `..` would resolve `link/..` back to `root`
        // (the parent of the `link` entry itself); the correct, OS-resolved
        // answer is `root/real` (the parent of what `link` actually points
        // at) -- exactly the case lexical normalization gets wrong.
        let root = tempfile::tempdir().unwrap();
        let real_child = root.path().join("real").join("child");
        std::fs::create_dir_all(&real_child).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&real_child, &link).unwrap();

        let resolved = absolute_path(&link.join("..")).unwrap();
        let expected = std::fs::canonicalize(root.path().join("real")).unwrap();
        assert_eq!(resolved, expected);
    }

    #[test]
    fn home_cache_dirs_are_bound_read_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".cargo")).unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args = build_bwrap_args_with_home(None, "true", &[], Some(&home), false);
        let cargo_path = path_to_string(&dir.path().join(".cargo"));
        let idx = args
            .iter()
            .position(|a| a == &cargo_path)
            .expect(".cargo cache dir bound");
        assert_eq!(args[idx - 1], "--ro-bind-try");
    }

    #[test]
    fn agentflare_data_dir_bound_read_write_when_present() {
        // Item #120: `~/.agentflare` holds the sqlite DB every `item`/
        // `comment`/`vent` MCP call writes through, so a dispatched job
        // needs write access to report its own completion -- read-only (or
        // missing entirely) means the job finishes real work with no way to
        // signal it, exactly what happened live on items #112 and #116.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(AGENTFLARE_DATA_DIR_RELATIVE)).unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args = build_bwrap_args_with_home(None, "true", &[], Some(&home), false);
        let agentflare_path = path_to_string(&dir.path().join(AGENTFLARE_DATA_DIR_RELATIVE));
        let idx = args
            .iter()
            .position(|a| a == &agentflare_path)
            .expect(".agentflare dir bound");
        assert_eq!(args[idx - 1], "--bind-try");
    }

    #[test]
    fn agentflare_data_dir_skipped_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args = build_bwrap_args_with_home(None, "true", &[], Some(&home), false);
        let agentflare_path = path_to_string(&dir.path().join(AGENTFLARE_DATA_DIR_RELATIVE));
        assert!(!args.iter().any(|a| a == &agentflare_path));
    }

    #[test]
    fn opencode_data_dir_overlaid_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join(OPENCODE_DATA_DIR_RELATIVE);
        std::fs::create_dir_all(&data_dir).unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args = build_bwrap_args_with_home(None, "/usr/bin/opencode", &[], Some(&home), false);
        let data_str = path_to_string(&data_dir);
        let src_idx = args
            .iter()
            .position(|a| a == "--overlay-src")
            .expect("--overlay-src present");
        assert_eq!(args[src_idx + 1], data_str);
        let overlay_idx = args
            .iter()
            .position(|a| a == "--tmp-overlay")
            .expect("--tmp-overlay present");
        assert_eq!(args[overlay_idx + 1], data_str);
    }

    #[test]
    fn opencode_data_dir_uses_tmpfs_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args = build_bwrap_args_with_home(None, "/usr/bin/opencode", &[], Some(&home), false);
        let data_str = path_to_string(&dir.path().join(OPENCODE_DATA_DIR_RELATIVE));
        let idx = args
            .iter()
            .position(|a| a == &data_str)
            .expect("opencode data dir tmpfs-mounted");
        assert_eq!(args[idx - 1], "--tmpfs");
        assert!(!args.iter().any(|a| a == "--overlay-src"));
    }

    #[test]
    fn non_opencode_command_gets_no_opencode_specific_mount() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(OPENCODE_DATA_DIR_RELATIVE)).unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args = build_bwrap_args_with_home(None, "/usr/bin/claude", &[], Some(&home), false);
        assert!(
            !args
                .iter()
                .any(|a| a == "--overlay-src" || a == "--tmp-overlay")
        );
    }

    #[test]
    fn claude_data_dir_overlaid_when_present() {
        // Item #127: `~/.claude` holds `.credentials.json`, which claude-code's
        // OAuth refresh must write (`.oauth_refresh.lock` + rotated-token
        // persist) -- read-only under the sandbox's root bind means the
        // refresh never completes and every dispatched job fails auth. A
        // claude-code command must get `~/.claude` overlaid, not left on the
        // read-only root.
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join(CLAUDE_DATA_DIR_RELATIVE);
        std::fs::create_dir_all(&data_dir).unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args =
            build_bwrap_args_with_home(None, "/usr/local/bin/claude", &[], Some(&home), false);
        let data_str = path_to_string(&data_dir);
        let src_idx = args
            .iter()
            .position(|a| a == "--overlay-src")
            .expect("--overlay-src present");
        assert_eq!(args[src_idx + 1], data_str);
        let overlay_idx = args
            .iter()
            .position(|a| a == "--tmp-overlay")
            .expect("--tmp-overlay present");
        assert_eq!(args[overlay_idx + 1], data_str);
    }

    #[test]
    fn claude_data_dir_uses_tmpfs_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args =
            build_bwrap_args_with_home(None, "/usr/local/bin/claude", &[], Some(&home), false);
        let data_str = path_to_string(&dir.path().join(CLAUDE_DATA_DIR_RELATIVE));
        let idx = args
            .iter()
            .position(|a| a == &data_str)
            .expect("claude data dir tmpfs-mounted");
        assert_eq!(args[idx - 1], "--tmpfs");
        assert!(!args.iter().any(|a| a == "--overlay-src"));
    }

    #[test]
    fn non_claude_command_gets_no_claude_specific_mount() {
        // The `~/.claude` overlay is gated on the command actually being
        // claude-code; an opencode job must not get one (opencode gets its own
        // dir instead).
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(CLAUDE_DATA_DIR_RELATIVE)).unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args = build_bwrap_args_with_home(None, "/usr/bin/opencode", &[], Some(&home), false);
        assert!(
            !args
                .iter()
                .any(|a| a == "--overlay-src" || a == "--tmp-overlay")
        );
    }

    #[test]
    fn cursor_data_dir_overlaid_when_present() {
        // Item #130: `~/.cursor` holds `mcp.json`/`hooks.json` plus a
        // per-project tracking dir cursor-agent creates on demand -- left
        // off the sandbox entirely (falling through to the read-only root),
        // that `mkdir` failed and every cursor-agent job died immediately.
        // A cursor command must get `~/.cursor` overlaid, not left on the
        // read-only root.
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join(CURSOR_DATA_DIR_RELATIVE);
        std::fs::create_dir_all(&data_dir).unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args = build_bwrap_args_with_home(
            None,
            "/usr/local/bin/cursor-agent",
            &[],
            Some(&home),
            false,
        );
        let data_str = path_to_string(&data_dir);
        let src_idx = args
            .iter()
            .position(|a| a == "--overlay-src")
            .expect("--overlay-src present");
        assert_eq!(args[src_idx + 1], data_str);
        let overlay_idx = args
            .iter()
            .position(|a| a == "--tmp-overlay")
            .expect("--tmp-overlay present");
        assert_eq!(args[overlay_idx + 1], data_str);
    }

    #[test]
    fn cursor_data_dir_uses_tmpfs_when_absent() {
        let dir = tempfile::tempdir().unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args = build_bwrap_args_with_home(
            None,
            "/usr/local/bin/cursor-agent",
            &[],
            Some(&home),
            false,
        );
        let data_str = path_to_string(&dir.path().join(CURSOR_DATA_DIR_RELATIVE));
        let idx = args
            .iter()
            .position(|a| a == &data_str)
            .expect("cursor data dir tmpfs-mounted");
        assert_eq!(args[idx - 1], "--tmpfs");
        assert!(!args.iter().any(|a| a == "--overlay-src"));
    }

    #[test]
    fn non_cursor_command_gets_no_cursor_specific_mount() {
        // The `~/.cursor` overlay is gated on the command actually being
        // cursor-agent; a claude/opencode job must not get one.
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(CURSOR_DATA_DIR_RELATIVE)).unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args = build_bwrap_args_with_home(None, "/usr/bin/opencode", &[], Some(&home), false);
        assert!(
            !args
                .iter()
                .any(|a| a == "--overlay-src" || a == "--tmp-overlay")
        );
    }
}
