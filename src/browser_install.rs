// First-use installer for the agent-browser sidecar via mise's git backend.
//
// `github:vercel-labs/agent-browser` is mise's UnifiedGitBackend: it installs
// prebuilt binaries straight from the git repo's GitHub releases
// (linux/macOS/Windows, no toolchain, no compile, no npm). Deliberately
// `mise install`, never `mise use -g`: install records nothing in the user's
// mise config (no global side effects) — the binary is resolved via
// `mise where` and invoked by absolute path, so shims never need activation.
// mise itself is bootstrapped through `mise_install` (curl|sh on unix,
// winget/scoop on windows) when absent.
use std::path::PathBuf;
use std::process::Stdio;

/// Git backend spec (floating version) + bare backend path for resolution.
const MISE_SPEC: &str = "github:vercel-labs/agent-browser@latest";
const MISE_BACKEND: &str = "github:vercel-labs/agent-browser";
/// Bound for stderr tails quoted in install errors (full logs stay on the
/// caller side; MCP responses must stay compact).
const ERR_TAIL_CHARS: usize = 2000;

/// Ensure the sidecar exists, auto-installing on first use when enabled.
/// Resolution order: `PATH`/cargo-bin hit (free) → cached `mise where`
/// result from a prior install in this process/session (free) → `mise
/// install` from the git backend (one-time download) → `agent-browser
/// install` (Chrome for Testing fetch). Concurrent first-use callers
/// serialize on a temp-dir lock; losers re-check and reuse the winner's
/// install. Returns the binary path — always absolute, so it works from
/// bare subprocess spawns in CLI and MCP alike.
///
/// The mise git backend deliberately never touches `PATH`/shims (see the
/// module doc), so `flare_browser::find_backend()` alone can never see a
/// mise-resolved install on a later call — without the path cache below,
/// *every* invocation (not just the first) would redo `mise install` +
/// `mise where` + the Chrome-for-Testing fetch.
pub fn ensure_agent_browser(auto_install: bool) -> Result<PathBuf, String> {
    if let Ok(p) = flare_browser::find_backend() {
        return Ok(p);
    }
    if let Some(p) = cached_bin_path() {
        return Ok(p);
    }
    if !auto_install {
        return Err(flare_browser::missing_hint());
    }
    let mise = match crate::mise_install::ensure_mise() {
        crate::mise_install::MiseOutcome::Present(bin)
        | crate::mise_install::MiseOutcome::Installed(bin) => bin,
        crate::mise_install::MiseOutcome::Failed(reason) => {
            return Err(format!("browser auto-install needs mise: {reason}"));
        }
    };
    let _lock = install_lock()?;
    // A racing caller may have finished while we waited on the lock.
    if let Ok(p) = flare_browser::find_backend() {
        return Ok(p);
    }
    if let Some(p) = cached_bin_path() {
        return Ok(p);
    }
    // flare_process::command: no bare Command::new for captured spawns —
    // suppresses the console-window flash on Windows when this runs
    // inside the daemon or an IDE-launched MCP server.
    let install_out = flare_process::command(&mise)
        .args(["install", MISE_SPEC])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to spawn `{mise} install`: {e}"))?;
    if !install_out.status.success() {
        return Err(format!(
            "`mise install {MISE_SPEC}` failed (exit {:?}): {}",
            install_out.status.code(),
            tail(&String::from_utf8_lossy(&install_out.stderr)),
        ));
    }
    let bin = resolve_via_mise_where(&mise)?;
    let chrome_out = flare_process::command(&bin)
        .arg("install")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to spawn `{} install`: {e}", bin.display()))?;
    if !chrome_out.status.success() {
        return Err(format!(
            "`{} install` (Chrome for Testing fetch) failed (exit {:?}): {}. On Linux this is usually missing system libs — retry with sudo: `{} install --with-deps`",
            bin.display(),
            chrome_out.status.code(),
            tail(&String::from_utf8_lossy(&chrome_out.stderr)),
            bin.display(),
        ));
    }
    write_bin_path_cache(&bin);
    Ok(bin)
}

/// Where the resolved absolute binary path is cached across invocations
/// (separate processes never share the `find_backend()` PATH-scan result,
/// and the mise git backend puts nothing on `PATH`).
fn bin_path_cache_file() -> PathBuf {
    std::env::temp_dir().join("agentflare-browser-bin-path")
}

/// Read the cached path from a prior install, if it still points at a real
/// file (self-heals if the install was moved/removed since).
fn cached_bin_path() -> Option<PathBuf> {
    let text = std::fs::read_to_string(bin_path_cache_file()).ok()?;
    let p = PathBuf::from(text.trim());
    p.is_file().then_some(p)
}

/// Best-effort: a failed cache write just means the next call redoes the
/// mise resolution, never a hard error for the caller who already has the
/// binary path in hand.
fn write_bin_path_cache(bin: &PathBuf) {
    let _ = std::fs::write(bin_path_cache_file(), bin.to_string_lossy().as_bytes());
}

fn resolve_via_mise_where(mise: &str) -> Result<PathBuf, String> {
    let out = flare_process::command(mise)
        .args(["where", MISE_BACKEND])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to spawn `{mise} where`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`mise where {MISE_BACKEND}` failed after a successful install (exit {:?}): {} — retry manually: `mise install {MISE_SPEC}`",
            out.status.code(),
            tail(&String::from_utf8_lossy(&out.stderr)),
        ));
    }
    parse_where_output(&String::from_utf8_lossy(&out.stdout))
}

/// `mise where` prints the install dir; the binary lives in `bin/` beneath
/// it (`agent-browser[.exe]`).
fn parse_where_output(stdout: &str) -> Result<PathBuf, String> {
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or_else(|| {
            format!("`mise where {MISE_BACKEND}` printed no path — retry manually: `mise install {MISE_SPEC}`")
        })?;
    let dir = PathBuf::from(line);
    let cand = dir.join("bin").join(flare_browser::BACKEND_BIN);
    if cand.is_file() {
        return Ok(cand);
    }
    #[cfg(windows)]
    {
        let exe = dir
            .join("bin")
            .join(format!("{}.exe", flare_browser::BACKEND_BIN));
        if exe.is_file() {
            return Ok(exe);
        }
    }
    Err(format!(
        "mise install dir {line} has no bin/agent-browser — retry manually: `mise install {MISE_SPEC}`"
    ))
}

fn tail(text: &str) -> String {
    let text = text.trim();
    if text.len() <= ERR_TAIL_CHARS {
        return text.to_string();
    }
    let mut start = text.len() - ERR_TAIL_CHARS;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &text[start..])
}

/// Cross-process mutex for first-use installs (temp dir needs no caller
/// config; the returned `File` guard releases the flock on drop).
fn install_lock() -> Result<std::fs::File, String> {
    use fs2::FileExt;
    let path = std::env::temp_dir().join("agentflare-browser-install.lock");
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NOFOLLOW: `temp_dir()` is a shared, world-writable directory on
        // Unix -- another local user could pre-plant a symlink at this path
        // pointing at a file only this process can write to. Refuse to
        // follow it rather than silently truncating/locking whatever it
        // points at.
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let file = opts
        .open(&path)
        .map_err(|e| format!("lock file {}: {e}", path.display()))?;
    file.lock_exclusive()
        .map_err(|e| format!("install lock {}: {e}", path.display()))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_where_resolves_bin_under_install_dir() {
        let dir = std::env::temp_dir().join("agentflare-browser-where-test");
        let bin_dir = dir.join("bin");
        std::fs::create_dir_all(&bin_dir).unwrap();
        let bin = bin_dir.join(flare_browser::BACKEND_BIN);
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        let out = format!("\n  \n{}\n", dir.display());
        assert_eq!(parse_where_output(&out).unwrap(), bin);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_where_rejects_empty_and_binless_dirs() {
        assert!(parse_where_output("   \n").is_err());
        let dir = std::env::temp_dir().join("agentflare-browser-where-test-empty");
        std::fs::create_dir_all(&dir).unwrap();
        assert!(parse_where_output(&dir.to_string_lossy()).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cached_bin_path_ignores_a_stale_or_missing_entry() {
        let cache = bin_path_cache_file();
        let saved = std::fs::read_to_string(&cache).ok();

        std::fs::remove_file(&cache).ok();
        assert!(cached_bin_path().is_none());

        std::fs::write(&cache, "/definitely/does/not/exist/agent-browser").unwrap();
        assert!(cached_bin_path().is_none(), "stale entries must not be trusted");

        let dir = std::env::temp_dir().join("agentflare-browser-cache-test");
        std::fs::create_dir_all(&dir).unwrap();
        let real_bin = dir.join(flare_browser::BACKEND_BIN);
        std::fs::write(&real_bin, "#!/bin/sh
").unwrap();
        write_bin_path_cache(&real_bin);
        assert_eq!(cached_bin_path(), Some(real_bin));
        std::fs::remove_dir_all(&dir).ok();

        match saved {
            Some(s) => std::fs::write(&cache, s).unwrap(),
            None => {
                std::fs::remove_file(&cache).ok();
            }
        }
    }

    #[test]
    fn tail_caps_long_stderr() {
        assert_eq!(tail("short"), "short");
        let long = "e".repeat(ERR_TAIL_CHARS + 100);
        let out = tail(&long);
        assert!(out.starts_with('…'));
        assert!(out.len() <= ERR_TAIL_CHARS + 10);
    }

    #[test]
    fn ensure_respects_disabled_auto_install_when_backend_absent() {
        // Also ignore a stale path cache from a real install on this
        // machine, so this test holds regardless of prior local state.
        std::fs::remove_file(bin_path_cache_file()).ok();
        // Scrub lookup roots so the test holds whether or not this machine
        // has the sidecar. SAFETY: restored below; nothing else in this
        // module reads PATH/CARGO_HOME/HOME.
        let saved_path = std::env::var_os("PATH");
        let saved_cargo = std::env::var_os("CARGO_HOME");
        let saved_home = std::env::var_os("HOME");
        let empty = std::env::temp_dir().join("agentflare-browser-test-empty");
        std::fs::create_dir_all(&empty).unwrap();
        unsafe {
            std::env::set_var("PATH", &empty);
            std::env::set_var("CARGO_HOME", &empty);
            std::env::set_var("HOME", &empty);
        }
        let err = ensure_agent_browser(false).unwrap_err();
        assert!(err.contains("mise install github:vercel-labs/agent-browser"), "{err}");
        unsafe {
            if let Some(v) = saved_path {
                std::env::set_var("PATH", v);
            }
            if let Some(v) = saved_cargo {
                std::env::set_var("CARGO_HOME", v);
            }
            if let Some(v) = saved_home {
                std::env::set_var("HOME", v);
            }
        }
    }
}
