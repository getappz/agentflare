//! PATH-shim installer for `agentflare init`: hardlinks (falling back to a
//! copy) the bundled `agentflare-shim` binary under every name in
//! `GENERIC_SHIM_TOOLS`, plus the dedicated `git` shim
//! (`crates/flare-git-shim`), into `~/.agentflare/shims/`.
//!
//! Source binaries are auto-discovered next to the currently-running
//! `agentflare` executable (`current_exe().parent()`) -- the release
//! archive is expected to bundle them there. A dev/cargo-install-only
//! setup that lacks them skips silently rather than erroring: PATH shims
//! are a nice-to-have (faster/compressed tool calls via lean-ctx), not
//! required for agentflare to function.

use crate::cli::git::{ensure_on_path, install_git_shim_binary, shim_dest_name, shims_dir};
use std::fs;
use std::path::{Path, PathBuf};

/// Every tool name the generic `agentflare-shim` binary stands in for.
/// One binary, hardlinked (falling back to a copy) under each name --
/// `git` is deliberately excluded here: it ships its own dedicated shim
/// with branch-guard/audit logic beyond generic passthrough, installed
/// separately below via `install_git_shim_binary`.
const GENERIC_SHIM_TOOLS: &[&str] = &[
    "aws",
    "biome",
    "bun",
    "bundle",
    "bunx",
    "cargo",
    "cat",
    "cmake",
    "composer",
    "curl",
    "deno",
    "df",
    "docker",
    "docker-compose",
    "dotnet",
    "du",
    "egrep",
    "eslint",
    "fgrep",
    "find",
    "gh",
    "go",
    "golangci-lint",
    "grep",
    "head",
    "helm",
    "kubectl",
    "ls",
    "make",
    "mix",
    "mypy",
    "npm",
    "php",
    "pip",
    "pip3",
    "pnpm",
    "prettier",
    "ps",
    "pytest",
    "python",
    "python3",
    "rake",
    "rg",
    "ruff",
    "swift",
    "tail",
    "terraform",
    "tofu",
    "tsc",
    "vite",
    "wc",
    "wget",
    "yarn",
    "zig",
];

fn exe_name(stem: &str) -> String {
    if cfg!(windows) {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

fn generic_shim_binary_name() -> String {
    exe_name("agentflare-shim")
}

/// The bundled generic shim binary, if the running install shipped one
/// next to the current `agentflare` executable.
fn bundled_generic_shim() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let path = dir.join(generic_shim_binary_name());
    path.exists().then_some(path)
}

/// The bundled `git` shim binary (see `crates/flare-git-shim`'s
/// `[[bin]] name = "git"`), if present next to the current executable.
fn bundled_git_shim() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let path = dir.join(shim_dest_name());
    path.exists().then_some(path)
}

/// `true` once every generic tool name plus `git` has a shim on disk --
/// presence only, not a content/hash check (matches this codebase's other
/// `Component::check` implementations, e.g. `rules`). An upgrade that
/// ships a newer shim binary needs a re-run with consent, same as `mise`.
pub fn all_shims_present() -> bool {
    let dir = shims_dir();
    GENERIC_SHIM_TOOLS
        .iter()
        .all(|name| dir.join(exe_name(name)).exists())
        && dir.join(shim_dest_name()).exists()
}

/// Hardlinks `src` to `dest` (same file on disk, no duplicated bytes),
/// falling back to a copy when hardlinking isn't available (cross-volume
/// install, restricted filesystem). Replaces `dest` if it already exists.
fn link_or_copy(src: &Path, dest: &Path) -> std::io::Result<()> {
    if dest.exists() {
        fs::remove_file(dest)?;
    }
    if fs::hard_link(src, dest).is_ok() {
        return Ok(());
    }
    fs::copy(src, dest).map(|_| ())
}

/// Installs every PATH shim this build has binaries for. Returns a status
/// message for `Component::apply`'s display.
pub fn install() -> String {
    let dir = shims_dir();
    if let Err(e) = fs::create_dir_all(&dir) {
        return format!("cannot create {}: {e}", dir.display());
    }

    let mut messages = Vec::new();

    match bundled_generic_shim() {
        Some(src) => {
            let mut installed = 0usize;
            let mut failed = Vec::new();
            for name in GENERIC_SHIM_TOOLS {
                match link_or_copy(&src, &dir.join(exe_name(name))) {
                    Ok(()) => installed += 1,
                    Err(e) => failed.push(format!("{name} ({e})")),
                }
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                for name in GENERIC_SHIM_TOOLS {
                    let _ = fs::set_permissions(
                        dir.join(exe_name(name)),
                        fs::Permissions::from_mode(0o755),
                    );
                }
            }
            if failed.is_empty() {
                messages.push(format!("{installed} generic tool shims linked"));
            } else {
                messages.push(format!(
                    "{installed} generic tool shims linked, {} failed: {}",
                    failed.len(),
                    failed.join(", ")
                ));
            }
        }
        None => messages.push(
            "no bundled agentflare-shim binary next to this executable — skipped generic tool shims"
                .to_string(),
        ),
    }

    match bundled_git_shim() {
        Some(src) => match install_git_shim_binary(&dir, &src) {
            Ok(dest) => messages.push(format!("git shim -> {}", dest.display())),
            Err(e) => messages.push(format!("git shim install failed: {e}")),
        },
        None => messages.push("no bundled git shim binary — skipped".to_string()),
    }

    match ensure_on_path(&dir) {
        Ok(true) => messages.push(format!(
            "added {} to PATH — restart your terminal/IDE to pick it up",
            dir.display()
        )),
        Ok(false) => {}
        Err(e) => messages.push(format!("could not update PATH: {e}")),
    }

    messages.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_or_copy_creates_a_working_hardlink_or_copy() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        fs::write(&src, b"shim payload").unwrap();
        let dest = dir.path().join("dest.bin");

        link_or_copy(&src, &dest).unwrap();

        assert_eq!(fs::read(&dest).unwrap(), b"shim payload");
    }

    #[test]
    fn link_or_copy_replaces_an_existing_dest() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        fs::write(&src, b"new content").unwrap();
        let dest = dir.path().join("dest.bin");
        fs::write(&dest, b"stale content").unwrap();

        link_or_copy(&src, &dest).unwrap();

        assert_eq!(fs::read(&dest).unwrap(), b"new content");
    }

    #[test]
    fn generic_shim_binary_name_has_platform_extension() {
        let name = generic_shim_binary_name();
        assert_eq!(cfg!(windows), name.ends_with(".exe"));
        assert!(name.starts_with("agentflare-shim"));
    }
}
