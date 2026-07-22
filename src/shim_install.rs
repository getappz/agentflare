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

pub(crate) fn generic_shim_binary_name() -> String {
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

/// `true` if `path` is already hardlinked to `src` (same file on disk) --
/// via the `same-file` crate, since Rust's std has no stable cross-platform
/// "same file" check (the Windows one, `MetadataExt::number_of_links`, is
/// still behind the unstable `windows_by_handle` feature).
fn is_linked_to(path: &Path, src: &Path) -> bool {
    same_file::is_same_file(path, src).unwrap_or(false)
}

/// `true` if hardlinking actually works between `dir` and `src`'s volume --
/// probed via a throwaway link rather than assumed, since a cross-volume
/// install or a restricted filesystem falls back to plain copies (see
/// `link_or_copy`), where `nlink > 1` is an unreachable target forever.
/// Without this probe, `all_shims_present` would treat a legitimately
/// copy-only install as perpetually stale and re-prompt on every `init`.
fn hardlinking_works(dir: &Path, src: &Path) -> bool {
    let probe = dir.join(".shim-hardlink-probe");
    let _ = fs::remove_file(&probe);
    let ok = fs::hard_link(src, &probe).is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

/// `true` once every generic tool name plus `git` has a shim on disk. For
/// the generic-tool group, a plain copy left behind by an install predating
/// hardlink support (or by the copy-fallback path) also counts as "not
/// done" whenever hardlinking is actually achievable on this filesystem --
/// so re-running `init` after upgrading to this version self-heals a
/// machine's existing duplicated-bytes shims into hardlinks instead of
/// leaving them as-is forever.
pub fn all_shims_present() -> bool {
    let dir = shims_dir();
    let generic_ok = GENERIC_SHIM_TOOLS.iter().all(|name| {
        let path = dir.join(exe_name(name));
        if !path.exists() {
            return false;
        }
        match bundled_generic_shim() {
            Some(src) => is_linked_to(&path, &src) || !hardlinking_works(&dir, &src),
            None => true,
        }
    });
    generic_ok && dir.join(shim_dest_name()).exists()
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

    #[test]
    fn is_linked_to_is_false_for_a_standalone_copy() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        fs::write(&src, b"x").unwrap();
        let copy = dir.path().join("standalone.bin");
        fs::copy(&src, &copy).unwrap();
        assert!(!is_linked_to(&copy, &src));
    }

    #[test]
    fn is_linked_to_is_true_once_hardlinked() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        fs::write(&src, b"x").unwrap();
        let dest = dir.path().join("linked.bin");
        fs::hard_link(&src, &dest).unwrap();
        assert!(is_linked_to(&dest, &src));
    }

    #[test]
    fn hardlinking_works_detects_same_volume_capability() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src.bin");
        fs::write(&src, b"x").unwrap();
        // Same-volume temp dir on every CI platform this crate targets --
        // this is a capability probe, not a hardlink-vs-copy race, so a
        // `true` result here is the expected common case.
        assert!(hardlinking_works(dir.path(), &src));
        // The probe file must be cleaned up, not left behind.
        assert!(!dir.path().join(".shim-hardlink-probe").exists());
    }

    #[test]
    fn all_shims_present_self_heals_a_stale_copy_into_a_hardlink() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join(generic_shim_binary_name());
        fs::write(&src, b"shim payload").unwrap();

        // A pre-existing plain copy (e.g. from before hardlinking landed)
        // must NOT read as "linked to src" -- confirming the building block
        // `all_shims_present` relies on actually distinguishes the two
        // cases, without needing the real `~/.agentflare/shims` layout.
        let copy_dest = dir.path().join("aws.bin");
        fs::copy(&src, &copy_dest).unwrap();
        assert!(!is_linked_to(&copy_dest, &src), "a plain copy isn't linked");

        let link_dest = dir.path().join("cargo.bin");
        link_or_copy(&src, &link_dest).unwrap();
        assert!(
            is_linked_to(&link_dest, &src) || !hardlinking_works(dir.path(), &src),
            "link_or_copy's hardlink path must register as linked"
        );
    }
}
