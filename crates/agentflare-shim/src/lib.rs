//! Generic PATH-shim exec plumbing, shared by any `agentflare-*` shim
//! binary: resolve the real target binary, exec it with argv/stdio
//! passthrough, and propagate its exit code. Tool-specific dispatch logic
//! (what to do BEFORE falling back to the real binary) lives in each shim
//! binary's own `main.rs`.

use std::env;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, exit};

/// True if the named env var is set to a non-empty value.
#[must_use]
pub fn is_set(name: &str) -> bool {
    env::var_os(name).is_some_and(|v| !v.is_empty())
}

/// Project marker directory: its presence means agentflare actually tracks
/// this project. Shared by every shim binary that needs to scope its
/// behavior to agentflare-managed projects only.
pub const PROJECT_MARKER: &str = ".agentflare";

/// Walk up from `start` looking for `.agentflare`, stopping at `home`
/// (exclusive) -- `~/.agentflare` is agentflare's own data dir, not a
/// project marker, and would otherwise false-positive on everything
/// under the user's home directory.
#[must_use]
pub fn in_scoped_project(start: &Path, home: Option<&Path>) -> bool {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if home.is_some_and(|h| h == d) {
            return false;
        }
        if d.join(PROJECT_MARKER).exists() {
            return true;
        }
        dir = d.parent();
    }
    false
}

/// Emits a trace line to stderr when `AGENTFLARE_SHIM_TRACE` is set.
pub fn trace(msg: &str) {
    if is_set("AGENTFLARE_SHIM_TRACE") {
        eprintln!("[flare-trace] {msg}");
    }
}

/// `true` if any two adjacent path components are "target" followed by
/// "debug"/"release" -- a cargo build-profile directory (and everything
/// under it, e.g. `target/debug/deps`, `target/debug/build/*/out`).
/// Cargo prepends this to PATH for every test/run process (so build-script
/// DLLs resolve), and any `[[bin]]` target in the same cargo workspace
/// lands directly in it -- so during development/testing, a shim binary
/// built via cargo can find ANOTHER shim binary (or a differently-pathed
/// copy of itself, e.g. `target/debug/deps/git.exe` alongside
/// `target/debug/git.exe`) there instead of the real target. Not a
/// concern for an installed shim (only ever one file in
/// `~/.agentflare/shims/`), but a real hazard under `cargo test`.
fn is_cargo_target_profile_dir(p: &Path) -> bool {
    let comps: Vec<_> = p.components().collect();
    comps.windows(2).any(|w| {
        w[0].as_os_str() == "target"
            && (w[1].as_os_str() == "debug" || w[1].as_os_str() == "release")
    })
}

/// Case-insensitive, separator-normalized path comparison. macOS (HFS+/
/// APFS) and Windows volumes are case-insensitive by default, so a byte-equal
/// `PathBuf` comparison can miss a real match when inputs differ only by
/// case or by `/` vs `\` -- and a missed match here means `shim_dir` leaks
/// back into the filtered PATH, so a shim binary's own real-binary lookup
/// resolves back to itself (see `flare_git_core::shell::git_binary`'s
/// identically-named helper, added in PR #304 for the same reason after
/// this exact bug class blocked `create_worktree`'s internal `git`
/// resolution -- this copy exists because `agentflare-shim` is dependency-
/// light generic plumbing shared by every shim binary and deliberately
/// doesn't depend on `flare-git-core`).
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

/// PATH with `shim_dir` removed, so a shim binary's own real-binary lookup
/// (and any child process it spawns) doesn't resolve back into itself --
/// also strips any cargo build-profile directory tree (see
/// `is_cargo_target_profile_dir`), which matters during development/
/// testing when multiple cargo-built binaries share one `target/debug`.
#[must_use]
pub fn path_without_shim_dir(shim_dir: &Path) -> Option<OsString> {
    let path_var = env::var_os("PATH")?;
    env::join_paths(
        env::split_paths(&path_var)
            .filter(|p| !paths_eq(p, shim_dir) && !is_cargo_target_profile_dir(p)),
    )
    .ok()
}

/// The tool name a shim binary is standing in for, derived from its own
/// filename (argv[0] / `current_exe`) -- e.g. a binary copied to `git` or
/// `git.exe` resolves to `"git"`.
pub fn tool_name_from_exe(exe: &Path) -> Option<String> {
    exe.file_stem().and_then(|s| s.to_str()).map(str::to_string)
}

/// Resolve `tool` on `filtered_path` (or the current PATH if `None`), exec
/// it with argv/stdio forwarded, and exit with its exit code. Exits 127 if
/// the tool can't be found or fails to spawn -- never returns.
pub fn run_real(tool: &str, filtered_path: Option<&OsString>, args: &[OsString]) -> ! {
    trace(&format!("real: {tool}"));
    let cwd = env::current_dir().unwrap_or_default();
    let resolved = match filtered_path {
        Some(p) => which::which_in(tool, Some(p), cwd),
        None => which::which(tool),
    };
    let Ok(real) = resolved else {
        eprintln!("agentflare-shim: command not found: {tool}");
        exit(127);
    };
    match Command::new(real).args(args).status() {
        Ok(status) => exit(status.code().unwrap_or(1)),
        Err(e) => {
            eprintln!("agentflare-shim: failed to exec {tool}: {e}");
            exit(127)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_eq_matches_case_variants_on_case_insensitive_platforms() {
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

    #[test]
    fn finds_marker_in_start_dir() {
        let tmp = std::env::temp_dir().join(format!("agentflare-shim-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join(PROJECT_MARKER), "").unwrap();
        assert!(in_scoped_project(&tmp, None));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn finds_marker_in_an_ancestor_dir() {
        let tmp =
            std::env::temp_dir().join(format!("agentflare-shim-test-anc-{}", std::process::id()));
        let sub = tmp.join("a").join("b");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(tmp.join(PROJECT_MARKER), "").unwrap();
        assert!(in_scoped_project(&sub, None));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn stops_at_home_without_treating_agentflares_own_dir_as_a_project_marker() {
        // ~/.agentflare is agentflare's own app-data dir, not a project
        // marker -- walking past `home` (inclusive of home itself) must
        // never false-positive on it. Regression for the bug the doc
        // comment on `in_scoped_project` calls out.
        let tmp =
            std::env::temp_dir().join(format!("agentflare-shim-test-home-{}", std::process::id()));
        let sub = tmp.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(tmp.join(PROJECT_MARKER), "").unwrap();
        assert!(!in_scoped_project(&sub, Some(&tmp)));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn no_marker_anywhere_is_not_scoped() {
        // Bound the walk-up with an explicit synthetic `home` one level
        // above `tmp`, rather than `None` -- an unbounded walk from a real
        // temp dir keeps climbing past this test's control (e.g. up into
        // the real machine's actual `~/.agentflare`, giving a false pass/fail
        // that has nothing to do with the logic under test).
        let tmp =
            std::env::temp_dir().join(format!("agentflare-shim-test-none-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert!(!in_scoped_project(&tmp, tmp.parent()));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
