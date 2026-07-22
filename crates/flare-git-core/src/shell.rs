//! Shared git-shelling primitives. Every module that needs to run `git`
//! against a repo goes through here instead of hand-rolling its own
//! `Command::new("git")` wrapper.

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

pub(crate) fn git_binary() -> PathBuf {
    static RESOLVED: OnceLock<PathBuf> = OnceLock::new();
    RESOLVED
        .get_or_init(|| {
            let self_dir = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(Path::to_path_buf));
            let shims_dir = agentflare_shims_dir();
            let filtered_path = std::env::var_os("PATH").map(|path_var| {
                std::env::join_paths(std::env::split_paths(&path_var).filter(|p| {
                    !self_dir.as_deref().is_some_and(|d| paths_eq(p, d))
                        && !shims_dir.as_deref().is_some_and(|d| paths_eq(p, d))
                        && !is_cargo_target_profile_dir(p)
                }))
                .unwrap_or(path_var)
            });
            let cwd = std::env::current_dir().unwrap_or_default();
            which::which_in("git", filtered_path.as_ref(), cwd)
                .unwrap_or_else(|_| PathBuf::from("git"))
        })
        .clone()
}

/// Runs `git` in `repo_root`; `Ok(stdout)` trimmed on success, `Err(stderr)`
/// trimmed on a non-zero exit, or a process-spawn error message (git
/// missing, etc) if it couldn't even run.
pub fn run_in(repo_root: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new(git_binary())
        .args(args)
        .current_dir(repo_root)
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

/// Unified diff for `base...head` (three-dot: changes on `head` since it
/// diverged from `base`). Stdout is returned RAW, not trimmed — diff output
/// is multi-line and whitespace-significant, unlike the single-value queries
/// the rest of this module's helpers return.
pub fn diff(repo_root: &Path, base: &str, head: &str) -> Result<String, String> {
    let range = format!("{base}...{head}");
    let out = Command::new(git_binary())
        .args(["diff", "--unified=3", &range])
        .current_dir(repo_root)
        .output()
        .map_err(|e| format!("git diff failed: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "git diff {range}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
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

    #[cfg(windows)]
    #[test]
    fn paths_eq_matches_separator_variants_on_windows() {
        let a = Path::new(r"C:\Users\shiva\.agentflare\shims");
        let b = Path::new("C:/Users/shiva/.agentflare/shims");
        assert!(paths_eq(a, b), "/ vs \\ differences must match on Windows");
    }
}
