//! Shared git-shelling primitives. Every module that needs to run `git`
//! against a repo goes through here instead of hand-rolling its own
//! `Command::new("git")` wrapper.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Resolves the real `git` binary, always excluding the currently-running
/// executable's own directory from the search.
///
/// This crate is also linked into `flare-git-shim`, a binary literally
/// named `git`/`git.exe`. On Windows, an unqualified `Command::new("git")`
/// resolves via `SearchPathW`, whose search order checks the CALLING
/// PROCESS's OWN DIRECTORY before PATH -- so inside that shim, a bare
/// "git" spawn resolves back to the shim itself and recurses without
/// limit (this happened once, during development: a single test run spun
/// up 10,000+ processes before it was caught). `which::which_in` does a
/// plain PATH-directory-listing search with no such self-referential
/// step, so resolving through it -- always excluding this process's own
/// directory -- is immune to the same failure mode regardless of which
/// binary this crate ends up linked into.
/// `true` for a cargo build-profile directory (`.../target/debug` or
/// `.../target/release`) -- Cargo prepends this to PATH for every test/run
/// process (so build-script DLLs resolve), and any `[[bin]]` target in the
/// same workspace lands directly in it. Excluding only "this process's own
/// directory" isn't enough: a workspace that redirects `target-dir`
/// globally (e.g. `~/.cargo/target`, as this repo's `~/.cargo/config.toml`
/// does for sccache) means EVERY crate's test binaries share that PATH
/// entry with `flare-git-shim`'s freshly-built `git.exe`. Detected
/// structurally (name is "debug"/"release", parent is named "target") so
/// it works regardless of where the target dir physically lives.
fn is_cargo_target_profile_dir(p: &Path) -> bool {
    let comps: Vec<_> = p.components().collect();
    comps.windows(2).any(|w| {
        w[0].as_os_str() == "target"
            && (w[1].as_os_str() == "debug" || w[1].as_os_str() == "release")
    })
}

/// `~/.agentflare/shims` -- the PATH-shim install dir (mirrored here since
/// this crate can't depend on the main `agentflare` crate's `shim_install`
/// module). Must be excluded from `git_binary()`'s search the same way
/// `self_dir` is: `ensure_on_path` (`src/cli/git.rs`) prepends this dir to
/// the user's persistent PATH, so an unfiltered search resolves straight
/// back to the `git` PATH shim -- which classifies `worktree` as
/// always-deny (see `classify.rs`), making agentflare's OWN worktree
/// creation (`create_worktree`, called from the `item` claim flow)
/// self-deadlock silently: the shim's denial looks like an ordinary git
/// error to the soft-fail-on-error caller, so no error ever surfaces.
fn agentflare_shims_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".agentflare").join("shims"))
}

/// Case-insensitive, separator-normalized path comparison. macOS (HFS+/APFS)
/// and Windows volumes are case-insensitive by default, so a byte-equal
/// `PathBuf` comparison can miss a real match when inputs differ only by
/// case or by `/` vs `\` -- and a missed match here means the shims dir (or
/// this process's own dir) leaks back into the filtered PATH, reproducing
/// the self-deadlock this function exists to prevent. Ported from mise's
/// `file::paths_eq` (`~/workspace/refs/mise/src/file.rs`), which solves the
/// identical problem for its own PATH shims.
/// Normalized key for PATH dedup; matches `paths_eq` semantics.
fn path_dedup_key(p: &Path) -> String {
    #[cfg(any(windows, target_os = "macos"))]
    {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect::<Vec<_>>()
            .join("\0")
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        p.to_string_lossy().into_owned()
    }
}

fn paths_eq(a: &Path, b: &Path) -> bool {
    #[cfg(any(windows, target_os = "macos"))]
    {
        let normalize =
            |c: std::path::Component<'_>| c.as_os_str().to_string_lossy().to_lowercase();
        a.components()
            .map(normalize)
            .eq(b.components().map(normalize))
    }
    #[cfg(all(not(windows), not(target_os = "macos")))]
    {
        a == b
    }
}

/// `git_binary()`'s resolved binary path, plus the filtered/deduped PATH used
/// to resolve it -- kept together so callers can apply the SAME filtered PATH
/// to the spawned process's environment, not just use it for resolution.
struct ResolvedGit {
    binary: PathBuf,
    /// `None` if PATH wasn't set at all in this process's environment (rare,
    /// but then there's nothing to filter or apply).
    filtered_path: Option<std::ffi::OsString>,
}

fn resolved_git() -> &'static ResolvedGit {
    static RESOLVED: OnceLock<ResolvedGit> = OnceLock::new();
    RESOLVED.get_or_init(|| {
        let self_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf));
        let shims_dir = agentflare_shims_dir();
        let filtered_path = std::env::var_os("PATH").map(|path_var| {
            // Dedup on top of the existing self/shims/cargo-target-dir
            // filtering: a PATH that's had the same entry prepended
            // repeatedly (e.g. by a long-running daemon re-running setup
            // logic across many dispatches) is exactly the shape of bloat
            // that can eventually push a child spawn's argv+envp past
            // ARG_MAX (see `run_in`'s doc comment) even though none of the
            // individual entries are themselves excludable.
            let mut seen: HashSet<String> = HashSet::new();
            std::env::join_paths(std::env::split_paths(&path_var).filter(|p| {
                !self_dir.as_deref().is_some_and(|d| paths_eq(p, d))
                    && !shims_dir.as_deref().is_some_and(|d| paths_eq(p, d))
                    && !is_cargo_target_profile_dir(p)
                    && seen.insert(path_dedup_key(p))
            }))
            .unwrap_or(path_var)
        });
        let cwd = std::env::current_dir().unwrap_or_default();
        let binary = which::which_in("git", filtered_path.as_ref(), cwd)
            .unwrap_or_else(|_| PathBuf::from("git"));
        ResolvedGit {
            binary,
            filtered_path,
        }
    })
}

pub(crate) fn git_binary() -> PathBuf {
    resolved_git().binary.clone()
}

/// Applies the same filtered/deduped PATH used to resolve `git_binary()` to
/// the spawned command's own environment. Without this, `Command::output()`
/// inherits the FULL parent environment by default -- including whatever
/// bloated, unfiltered PATH the daemon process itself is carrying -- so a
/// spawn can still hit `E2BIG` even though `git_binary()` already computed a
/// clean PATH, because that clean PATH was only ever used to *locate* the
/// binary, never applied to the child's actual env.
pub(crate) fn apply_filtered_path(cmd: &mut Command) {
    if let Some(path) = &resolved_git().filtered_path {
        cmd.env("PATH", path);
    }
}

/// This crate's git spawns run inside the agentflare daemon far more than
/// any other one -- `resolve_project()` calls `run_in`-backed `repo_toplevel`
/// on essentially every project/item MCP call. The daemon itself is
/// console-less, so without the no-window flag every one of those spawns
/// auto-allocates a console window on Windows, flashing briefly.
fn no_console_window(cmd: &mut Command) {
    flare_process::no_window(cmd);
}

/// Runs `git` in `repo_root`; `Ok(stdout)` trimmed on success, `Err(stderr)`
/// trimmed on a non-zero exit, or a process-spawn error message (git
/// missing, etc) if it couldn't even run.
pub fn run_in(repo_root: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new(git_binary());
    cmd.args(args).current_dir(repo_root);
    no_console_window(&mut cmd);
    apply_filtered_path(&mut cmd);
    let out = cmd
        .output()
        .map_err(|e| format!("git not available: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `run_in`, discarding the error and treating empty stdout as `None` — the
/// "best-effort, don't care why it failed" shape most callers actually want.
#[must_use]
pub fn run_in_opt(repo_root: &Path, args: &[&str]) -> Option<String> {
    run_in(repo_root, args).ok().filter(|s| !s.is_empty())
}

/// `true` if `git <args>` exits 0 in `repo_root`; stdout/stderr don't matter.
#[must_use]
pub fn run_in_ok(repo_root: &Path, args: &[&str]) -> bool {
    run_in(repo_root, args).is_ok()
}

/// Runs `git worktree prune` when `needed`, clearing dangling
/// `.git/worktrees/<name>` admin entries left behind after a worktree
/// directory was removed some other way (e.g. `remove_dir_all`, or because
/// it was already gone). Best-effort: errors are silently ignored, matching
/// every existing call site. Shared by `worktree::gc_orphans` and
/// `doctor::reclaim`, which both batch removals and prune once at the end
/// rather than after every single deletion.
pub fn prune_worktree_metadata_if(repo_root: &Path, needed: bool) {
    if needed {
        let _ = run_in(repo_root, &["worktree", "prune"]);
    }
}

/// Unified diff for `base...head` (three-dot: changes on `head` since it
/// diverged from `base`). Stdout is returned RAW, not trimmed — diff output
/// is multi-line and whitespace-significant, unlike the single-value queries
/// the rest of this module's helpers return.
pub fn diff(repo_root: &Path, base: &str, head: &str) -> Result<String, String> {
    let range = format!("{base}...{head}");
    let mut cmd = Command::new(git_binary());
    cmd.args(["diff", "--unified=3", &range])
        .current_dir(repo_root);
    no_console_window(&mut cmd);
    apply_filtered_path(&mut cmd);
    let out = cmd.output().map_err(|e| format!("git diff failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git diff {range}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Why [`run_in_lines_bounded`] came back with an error.
#[derive(Debug)]
pub enum BoundedLinesError {
    /// git itself failed: could not spawn, or exited non-zero (stderr).
    Git(String),
    /// The output had more than the requested cap of non-empty lines.
    TooManyLines,
}

/// Runs `git <args>` in `repo_root`, streaming stdout line-by-line and
/// keeping only the first `cap` non-empty lines.
///
/// Bounds peak memory for outputs that would otherwise materialize as one
/// multi-MB string plus a `Vec<String>` clone of it — the pathological case
/// behind the scope-check child-process OOM (item #472): `git diff
/// <default>...<head> --name-only` on a branch that diverged massively from
/// the default branch lists the whole tree, and `Command::output()` buffers
/// it all before we can even count the lines. Streaming keeps peak usage at
/// the capped `Vec` plus one line instead.
///
/// `Err(BoundedLinesError::TooManyLines)` once `cap` lines are collected —
/// the child is killed rather than left to block forever on a full stdout
/// pipe (git is read-only here, so killing it loses nothing).
pub fn run_in_lines_bounded(
    repo_root: &Path,
    args: &[&str],
    cap: usize,
) -> Result<Vec<String>, BoundedLinesError> {
    use std::io::{BufRead, BufReader, Read};
    use std::process::Stdio;

    let mut cmd = Command::new(git_binary());
    cmd.args(args)
        .current_dir(repo_root)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    no_console_window(&mut cmd);
    apply_filtered_path(&mut cmd);
    let mut child = cmd
        .spawn()
        .map_err(|e| BoundedLinesError::Git(format!("git not available: {e}")))?;
    // Drain stderr on its own thread: reading stdout to EOF first would
    // deadlock if git fills the stderr pipe buffer (~64 KiB) while this
    // still-piped stdout read is blocked waiting for more lines.
    let stderr_handle = child.stderr.take().map(|mut e| {
        std::thread::spawn(move || {
            let mut buf = String::new();
            let _ = e.read_to_string(&mut buf);
            buf
        })
    });
    let mut lines = Vec::new();
    let mut exceeded = false;
    let mut read_err = None;
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    read_err = Some(format!("reading git output failed: {e}"));
                    break;
                }
            };
            if line.is_empty() {
                continue;
            }
            if lines.len() >= cap {
                exceeded = true;
                break;
            }
            lines.push(line);
        }
    }
    if exceeded || read_err.is_some() {
        // Drain the pipe, else the child blocks forever on a full stdout
        // buffer and `wait()` never returns. Killing a read-only `git diff`
        // is safe.
        let _ = child.kill();
        let _ = child.wait();
        if let Some(e) = read_err {
            return Err(BoundedLinesError::Git(e));
        }
        return Err(BoundedLinesError::TooManyLines);
    }
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let status = child
        .wait()
        .map_err(|e| BoundedLinesError::Git(format!("waiting for git failed: {e}")))?;
    if !status.success() {
        return Err(BoundedLinesError::Git(stderr.trim().to_string()));
    }
    Ok(lines)
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::run_in;
    use std::path::PathBuf;
    use tempfile::TempDir;

    pub struct Repo {
        _dir: TempDir,
        pub path: PathBuf,
    }

    pub fn init_repo_with_branch(branch: &str) -> Repo {
        let dir = TempDir::new().unwrap();
        let path = dir.path().to_path_buf();
        run_in(&path, &["init", "-b", branch]).unwrap();
        run_in(&path, &["config", "user.email", "test@test.com"]).unwrap();
        run_in(&path, &["config", "user.name", "Test"]).unwrap();
        run_in(&path, &["commit", "--allow-empty", "-m", "initial"]).unwrap();
        Repo { _dir: dir, path }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::init_repo_with_branch;
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn run_in_opt_is_none_outside_a_repo() {
        let dir = TempDir::new().unwrap();
        assert!(run_in_opt(dir.path(), &["rev-parse", "--abbrev-ref", "HEAD"]).is_none());
    }

    #[test]
    fn run_in_ok_reflects_exit_status() {
        let repo = init_repo_with_branch("master");
        assert!(run_in_ok(&repo.path, &["rev-parse", "--verify", "master"]));
        assert!(!run_in_ok(
            &repo.path,
            &["rev-parse", "--verify", "no-such-branch"]
        ));
    }

    #[test]
    fn diff_returns_untrimmed_output_across_a_change() {
        let repo = init_repo_with_branch("master");
        std::fs::write(repo.path.join("f.txt"), "hello\n").unwrap();
        run_in(&repo.path, &["add", "f.txt"]).unwrap();
        run_in(&repo.path, &["commit", "-m", "add f.txt"]).unwrap();
        let out = diff(&repo.path, "HEAD~1", "HEAD").unwrap();
        assert!(out.contains("+hello"), "{out}");
    }

    #[test]
    fn diff_reports_git_stderr_on_an_invalid_range() {
        let repo = init_repo_with_branch("master");
        let err = diff(&repo.path, "no-such-branch", "HEAD").unwrap_err();
        assert!(err.contains("no-such-branch"), "{err}");
    }

    #[test]
    fn run_in_lines_bounded_returns_lines_up_to_the_cap() {
        let repo = init_repo_with_branch("master");
        for n in 1..=5 {
            std::fs::write(repo.path.join(format!("f{n}.txt")), "x\n").unwrap();
            run_in(&repo.path, &["add", format!("f{n}.txt").as_str()]).unwrap();
        }
        run_in(&repo.path, &["commit", "-m", "add five"]).unwrap();
        let lines =
            run_in_lines_bounded(&repo.path, &["diff", "--cached", "--name-only"], 5).unwrap();
        assert!(
            lines.is_empty(),
            "clean tree has no staged paths: {lines:?}"
        );
        let all = run_in_lines_bounded(&repo.path, &["ls-files"], 10).unwrap();
        assert_eq!(all.len(), 5, "{all:?}");
    }

    #[test]
    fn run_in_lines_bounded_errors_when_cap_is_exceeded() {
        let repo = init_repo_with_branch("master");
        for n in 1..=10 {
            std::fs::write(repo.path.join(format!("f{n}.txt")), "x\n").unwrap();
            run_in(&repo.path, &["add", format!("f{n}.txt").as_str()]).unwrap();
        }
        run_in(&repo.path, &["commit", "-m", "add ten"]).unwrap();
        assert!(
            matches!(
                run_in_lines_bounded(&repo.path, &["ls-files"], 5),
                Err(BoundedLinesError::TooManyLines)
            ),
            "more tracked files than the cap must error, not silently truncate"
        );
        let ok = run_in_lines_bounded(&repo.path, &["ls-files"], 10).unwrap();
        assert_eq!(ok.len(), 10, "{ok:?}");
    }

    #[test]
    fn run_in_lines_bounded_propagates_git_errors() {
        let repo = init_repo_with_branch("master");
        match run_in_lines_bounded(&repo.path, &["rev-parse", "--verify", "no-such-branch"], 10) {
            Err(BoundedLinesError::Git(e)) => {
                assert!(e.contains("Needed a single revision"), "{e}")
            }
            other => panic!("expected a git error, got {other:?}"),
        }
    }

    #[test]
    fn agentflare_shims_dir_is_excludable_from_git_binary_search() {
        // git_binary() must filter this exact path out of PATH, or a
        // shims-first PATH resolves "git" back to the shim, which denies
        // `worktree` unconditionally -- silently deadlocking agentflare's
        // own create_worktree.
        let dir = agentflare_shims_dir().expect("home dir resolvable in test env");
        assert!(dir.ends_with(std::path::Path::new(".agentflare").join("shims")));
    }

    #[test]
    fn paths_eq_matches_case_variants_on_case_insensitive_platforms() {
        // Forward slashes only -- macOS never treats `\` as a separator, so
        // a backslash path would split into components differently there.
        // Separator normalization is Windows-specific (see below).
        let a = Path::new("/Users/shiva/.agentflare/shims");
        let b = Path::new("/Users/shiva/.AGENTFLARE/shims");
        #[cfg(any(windows, target_os = "macos"))]
        assert!(paths_eq(a, b), "case-only differences must match");
        #[cfg(all(not(windows), not(target_os = "macos")))]
        assert!(
            !paths_eq(a, b),
            "byte-equal comparison on case-sensitive platforms"
        );

        assert!(!paths_eq(
            Path::new("/home/user/.agentflare/shims"),
            Path::new("/home/user/.cargo/bin")
        ));
    }

    #[test]
    fn apply_filtered_path_overrides_the_spawned_command_s_path_env() {
        let mut cmd = Command::new(git_binary());
        apply_filtered_path(&mut cmd);
        let overridden = cmd
            .get_envs()
            .find(|(k, _)| *k == std::ffi::OsStr::new("PATH"))
            .and_then(|(_, v)| v);
        assert_eq!(
            overridden,
            resolved_git().filtered_path.as_deref(),
            "run_in/diff must ship the SAME filtered PATH that resolved git_binary(), not the daemon's raw inherited one"
        );
    }

    // `resolved_git()` is a process-wide `OnceLock`, so this test re-execs the
    // compiled test binary fresh: the child inflates its OWN PATH with
    // thousands of duplicate entries -- the shape a long-running daemon
    // accumulates across many dispatches (see `resolved_git`'s doc comment)
    // -- then exercises `run_in`. Without `apply_filtered_path` wired into
    // `run_in`, the spawned git process inherits that raw bloated PATH and
    // dies with E2BIG; that's the regression this item was filed for.
    #[cfg(unix)]
    #[test]
    fn run_in_survives_a_path_bloated_by_repeated_daemon_dispatches() {
        const MARKER: &str = "SHELL_RS_E2BIG_REGRESSION_CHILD";
        const TEST_NAME: &str =
            "shell::tests::run_in_survives_a_path_bloated_by_repeated_daemon_dispatches";

        if std::env::var_os(MARKER).is_some() {
            let real_path = std::env::var_os("PATH").unwrap_or_default();
            let junk_dir = PathBuf::from(
                "/nonexistent/padding-directory-repeated-until-argv-plus-envp-overflow-e2big",
            );
            let mut entries: Vec<PathBuf> = std::env::split_paths(&real_path).collect();
            entries.extend(std::iter::repeat_n(junk_dir, 200_000));
            // Safe: this process was just re-exec'd solely to run this one
            // test and hasn't spawned any other threads yet.
            unsafe {
                std::env::set_var("PATH", std::env::join_paths(&entries).unwrap());
            }

            let repo = init_repo_with_branch("master");
            let ok = run_in(&repo.path, &["rev-parse", "--verify", "master"]).is_ok();
            std::process::exit(if ok { 0 } else { 1 });
        }

        let exe = std::env::current_exe().unwrap();
        let output = Command::new(&exe)
            .arg(TEST_NAME)
            .arg("--exact")
            .env(MARKER, "1")
            .output()
            .expect("failed to re-exec test binary");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("running 1 test"),
            "expected --exact to select exactly this test, got:\n{stdout}"
        );
        assert!(
            output.status.success(),
            "run_in should filter the daemon's bloated PATH before spawning git, not inherit it raw (E2BIG); child output:\n{stdout}"
        );
    }

    #[cfg(windows)]
    #[test]
    fn paths_eq_matches_separator_variants_on_windows() {
        let a = Path::new(r"C:\Users\shiva\.agentflare\shims");
        let b = Path::new("C:/Users/shiva/.agentflare/shims");
        assert!(paths_eq(a, b), "/ vs \\ differences must match on Windows");
    }
}
