//! Bubblewrap-based sandboxing for job commands on native Linux and WSL2.
//!
//! Flag set and ordering are adapted from openai/codex's `linux-sandbox`
//! crate (`bwrap.rs`): read-only root by default (`--ro-bind / /`), an
//! explicit writable bind for the job's cwd, `.git` re-protected read-only
//! even though it sits inside that writable root, and `--chdir` into the
//! cwd rather than relying on bubblewrap inheriting a possibly-symlinked
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

pub(super) fn wrap(
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
) -> Option<(String, Vec<String>)> {
    let bwrap = find_or_install_bwrap()?;
    Some((path_to_string(&bwrap), build_bwrap_args(cwd, command, args)))
}

fn build_bwrap_args(cwd: Option<&Path>, command: &str, args: &[String]) -> Vec<String> {
    build_bwrap_args_with_home(cwd, command, args, std::env::var_os("HOME").as_deref())
}

fn build_bwrap_args_with_home(
    cwd: Option<&Path>,
    command: &str,
    args: &[String],
    home: Option<&std::ffi::OsStr>,
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
        let cwd = absolute_path(cwd);
        let cwd_str = path_to_string(&cwd);
        bwrap_args.push("--bind".to_string());
        bwrap_args.push(cwd_str.clone());
        bwrap_args.push(cwd_str.clone());

        let git_dir = cwd.join(".git");
        if git_dir.exists() {
            let git_str = path_to_string(&git_dir);
            bwrap_args.push("--ro-bind".to_string());
            bwrap_args.push(git_str.clone());
            bwrap_args.push(git_str);
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
    }

    bwrap_args.push("--".to_string());
    bwrap_args.push(command.to_string());
    bwrap_args.extend(args.iter().cloned());

    bwrap_args
}

/// Resolves `path` to absolute against the current process's cwd when it
/// isn't already one. bwrap's own `--bind`/`--chdir` source paths are taken
/// literally, unlike `std::process::Command::current_dir`, which resolves a
/// relative path against the parent's cwd before handing it to the child --
/// without this, a relative job `cwd` would bind whatever bwrap's own
/// (unrelated) working directory happens to be, not the job's intended one.
fn absolute_path(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|base| base.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    normalize_lexically(&joined)
}

/// Collapses `.` and `..` components without touching the filesystem (no
/// symlink resolution) -- e.g. `/a/./b` -> `/a/b`. A literal `cwd: "."`
/// would otherwise reach bwrap as `<absolute-dir>/.`, which is harmless to
/// the kernel but pointless clutter in the sandbox's own bind args.
fn normalize_lexically(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
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
    fn relative_cwd_is_normalized_to_absolute_bind() {
        let args = build_bwrap_args_with_home(Some(Path::new(".")), "true", &[], None);
        let expected = path_to_string(&std::env::current_dir().unwrap());
        let (src, dest) = bind_arg(&args);
        assert_eq!(src, expected);
        assert_eq!(dest, expected);
    }

    #[test]
    fn nested_relative_cwd_is_normalized_to_absolute_bind() {
        let args = build_bwrap_args_with_home(Some(Path::new("src")), "true", &[], None);
        let expected = path_to_string(&std::env::current_dir().unwrap().join("src"));
        let (src, _) = bind_arg(&args);
        assert_eq!(src, expected);
    }

    #[test]
    fn absolute_cwd_is_left_unchanged() {
        let abs = std::env::current_dir().unwrap();
        let args = build_bwrap_args_with_home(Some(abs.as_path()), "true", &[], None);
        let expected = path_to_string(&abs);
        let (src, _) = bind_arg(&args);
        assert_eq!(src, expected);
    }

    #[test]
    fn home_cache_dirs_are_bound_read_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".cargo")).unwrap();
        let home = std::ffi::OsString::from(dir.path());
        let args = build_bwrap_args_with_home(None, "true", &[], Some(&home));
        let cargo_path = path_to_string(&dir.path().join(".cargo"));
        let idx = args
            .iter()
            .position(|a| a == &cargo_path)
            .expect(".cargo cache dir bound");
        assert_eq!(args[idx - 1], "--ro-bind-try");
    }
}
