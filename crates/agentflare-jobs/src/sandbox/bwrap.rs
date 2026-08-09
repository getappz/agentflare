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
use std::path::{Path, PathBuf};

/// $HOME subdirectories commonly needed by build/package tooling (cargo,
/// rustup, npm, pip caches) that would otherwise sit outside the job's cwd
/// and fail under the read-only root. Bound with `--bind-try` so a missing
/// one is silently skipped rather than failing sandbox setup.
const HOME_CACHE_DIRS: &[&str] = &[".cargo", ".rustup", ".cache", ".npm"];

pub(super) fn wrap(
    command: &str,
    args: &[String],
    cwd: Option<&Path>,
) -> Option<(String, Vec<String>)> {
    let bwrap = which_bwrap()?;

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
        if git_dir.exists() {
            let git_str = path_to_string(&git_dir);
            bwrap_args.push("--ro-bind".to_string());
            bwrap_args.push(git_str.clone());
            bwrap_args.push(git_str);
        }

        bwrap_args.push("--chdir".to_string());
        bwrap_args.push(cwd_str);
    }

    if let Some(home) = std::env::var_os("HOME") {
        for cache in HOME_CACHE_DIRS {
            let path = Path::new(&home).join(cache);
            if path.exists() {
                let path_str = path_to_string(&path);
                bwrap_args.push("--bind-try".to_string());
                bwrap_args.push(path_str.clone());
                bwrap_args.push(path_str);
            }
        }
    }

    bwrap_args.push("--".to_string());
    bwrap_args.push(command.to_string());
    bwrap_args.extend(args.iter().cloned());

    Some((path_to_string(&bwrap), bwrap_args))
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Minimal `PATH` scan for `bwrap`. This crate has no `which` dependency, and
/// `Command::new("bwrap")` alone won't tell us up front whether it's missing
/// -- checking here lets a job fall back to running unsandboxed (rather than
/// failing to spawn at all) when a box hasn't installed bubblewrap yet.
fn which_bwrap() -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join("bwrap"))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_bwrap_falls_back_to_none() {
        // SAFETY: single-threaded test process; no other test reads PATH here.
        unsafe {
            std::env::set_var("PATH", "");
        }
        assert!(which_bwrap().is_none());
    }
}
