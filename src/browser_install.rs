// First-use installer for the agent-browser sidecar via mise's git backend.
//
// `github:vercel-labs/agent-browser` is mise's UnifiedGitBackend: it installs
// prebuilt binaries straight from the git repo's GitHub releases
// (linux/macOS/Windows, no toolchain, no compile, no npm). Deliberately
// `mise install`, never `mise use -g`: install records nothing in the user's
// mise config (no global side effects) — the binary is resolved via `mise
// env --json` and invoked by absolute path, so shims never need activation.
// The git backend's install layout is flat (the binary sits directly in the
// install dir, no `bin/` subdir), so `mise env --json`'s merged `PATH` is
// scanned rather than guessed at; that same `PATH` is carried along and
// merged into the spawned agent-browser command's env, in case a future
// backend needs sibling tools on `PATH` at runtime. mise itself is
// bootstrapped through `mise_install` (curl|sh on unix, winget/scoop on
// windows) when absent.
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Git backend spec (floating version), used both to install and to
/// resolve the installed binary + its env via `mise env --json`.
const MISE_SPEC: &str = "github:vercel-labs/agent-browser@latest";
/// Bound for stderr tails quoted in install errors (full logs stay on the
/// caller side; MCP responses must stay compact).
const ERR_TAIL_CHARS: usize = 2000;

/// Ensure the sidecar exists, auto-installing on first use when enabled.
/// Resolution order: `PATH`/cargo-bin hit (free) → cached `mise env --json`
/// result from a prior install in this process/session (free) → `mise
/// install` from the git backend (one-time download) → `agent-browser
/// install` (Chrome for Testing fetch). Concurrent first-use callers
/// serialize on a temp-dir lock; losers re-check and reuse the winner's
/// install. Returns the binary path — always absolute, so it works from
/// bare subprocess spawns in CLI and MCP alike — plus the `PATH` mise
/// resolved for the spec, when resolution went through mise (`None` when
/// the binary was already reachable via the caller's own `PATH`/cargo-bin,
/// which needs no extra env to run). Callers should merge that `PATH` into
/// the env of any command they spawn against the returned binary.
///
/// The mise git backend deliberately never touches `PATH`/shims (see the
/// module doc), so `flare_browser::find_backend()` alone can never see a
/// mise-resolved install on a later call — without the cache below,
/// *every* invocation (not just the first) would redo `mise install` +
/// `mise env --json` + the Chrome-for-Testing fetch.
pub fn ensure_agent_browser(auto_install: bool) -> Result<(PathBuf, Option<String>), String> {
    if let Ok(p) = flare_browser::find_backend() {
        return Ok((p, None));
    }
    if let Some((bin, path_env)) = cached_backend() {
        return Ok((bin, Some(path_env)));
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
        return Ok((p, None));
    }
    if let Some((bin, path_env)) = cached_backend() {
        return Ok((bin, Some(path_env)));
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
    let (bin, path_env) = resolve_via_mise_env(&mise)?;
    let chrome_out = flare_process::command(&bin)
        .arg("install")
        .env("PATH", &path_env)
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
    write_backend_cache(&bin, &path_env);
    Ok((bin, Some(path_env)))
}

/// True when [`ensure_agent_browser`] would actually run `mise install`
/// (neither `find_backend()`'s `PATH` scan nor the cached prior-install file
/// resolves). Callers use this to gate a "not found, installing…" progress
/// message so it only fires on the real first-use path -- checking
/// `find_backend()` alone fires on every call, since the mise git backend
/// deliberately never touches `PATH` (see the module doc).
pub fn needs_install() -> bool {
    flare_browser::find_backend().is_err() && cached_backend().is_none()
}

/// Where the resolved binary path + its mise `PATH` are cached across
/// invocations (separate processes never share the `find_backend()`
/// PATH-scan result, and the mise git backend puts nothing on `PATH`).
fn cached_backend_file() -> PathBuf {
    std::env::temp_dir().join("agentflare-browser-bin-path")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedBackend {
    bin: PathBuf,
    path_env: String,
}

/// Read the cached backend from a prior install, if the binary still
/// points at a real file (self-heals if the install was moved/removed
/// since).
fn cached_backend() -> Option<(PathBuf, String)> {
    let text = std::fs::read_to_string(cached_backend_file()).ok()?;
    let cached: CachedBackend = serde_json::from_str(&text).ok()?;
    cached
        .bin
        .is_file()
        .then_some((cached.bin, cached.path_env))
}

/// Best-effort: a failed cache write just means the next call redoes the
/// mise resolution, never a hard error for the caller who already has the
/// binary path in hand.
fn write_backend_cache(bin: &Path, path_env: &str) {
    let cached = CachedBackend {
        bin: bin.to_path_buf(),
        path_env: path_env.to_string(),
    };
    if let Ok(text) = serde_json::to_string(&cached) {
        let _ = std::fs::write(cached_backend_file(), text);
    }
}

/// `mise env --json <spec>` resolves the full env mise would export to
/// activate the spec — no `mise where` + guessed `bin/` layout, which the
/// git backend doesn't even have (its binary sits directly in the install
/// dir). The returned `PATH` already has that install dir merged in ahead
/// of the caller's own `PATH`, so it's both how the binary is located here
/// and what gets carried along to merge into the spawned agent-browser
/// command's env.
fn resolve_via_mise_env(mise: &str) -> Result<(PathBuf, String), String> {
    let out = flare_process::command(mise)
        .args(["env", "--json", MISE_SPEC])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to spawn `{mise} env --json {MISE_SPEC}`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`mise env --json {MISE_SPEC}` failed after a successful install (exit {:?}): {} — retry manually: `mise install {MISE_SPEC}`",
            out.status.code(),
            tail(&String::from_utf8_lossy(&out.stderr)),
        ));
    }
    parse_env_output(&String::from_utf8_lossy(&out.stdout))
}

fn parse_env_output(stdout: &str) -> Result<(PathBuf, String), String> {
    let vars: std::collections::HashMap<String, String> = serde_json::from_str(stdout)
        .map_err(|e| format!("`mise env --json {MISE_SPEC}` produced invalid JSON: {e}"))?;
    let path = vars.get("PATH").cloned().ok_or_else(|| {
        format!(
            "`mise env --json {MISE_SPEC}` had no PATH — retry manually: `mise install {MISE_SPEC}`"
        )
    })?;
    let bin = std::env::split_paths(&path)
        .find_map(|dir| {
            let cand = dir.join(flare_browser::BACKEND_BIN);
            if cand.is_file() {
                return Some(cand);
            }
            #[cfg(windows)]
            {
                let exe = dir.join(format!("{}.exe", flare_browser::BACKEND_BIN));
                if exe.is_file() {
                    return Some(exe);
                }
            }
            None
        })
        .ok_or_else(|| {
            format!(
                "mise env for {MISE_SPEC} has no {} on PATH — retry manually: `mise install {MISE_SPEC}`",
                flare_browser::BACKEND_BIN
            )
        })?;
    Ok((bin, path))
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
    fn parse_env_finds_bin_flat_in_an_install_dir_on_path() {
        // The git backend's install dir has no `bin/` subdir -- the binary
        // sits directly in it -- so this must resolve without one.
        let dir = std::env::temp_dir().join("agentflare-browser-env-test");
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join(flare_browser::BACKEND_BIN);
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        // Build the fake PATH with the platform's own separator/joining
        // rules (`:` on Unix, `;` on Windows) -- a hardcoded Unix-style
        // literal made this test fail on Windows (`split_paths` couldn't
        // parse it), even though the function under test is platform-correct.
        let missing_a = std::env::temp_dir().join("agentflare-browser-env-test-missing-a");
        let missing_b = std::env::temp_dir().join("agentflare-browser-env-test-missing-b");
        let path = std::env::join_paths([&missing_a, &dir, &missing_b])
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let json = serde_json::json!({"PATH": path}).to_string();
        let (resolved_bin, resolved_path) = parse_env_output(&json).unwrap();
        assert_eq!(resolved_bin, bin);
        assert_eq!(resolved_path, path);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parse_env_rejects_invalid_json_missing_path_and_binless_path() {
        assert!(parse_env_output("not json").is_err());
        assert!(parse_env_output(r#"{"OTHER":"x"}"#).is_err());
        let dir = std::env::temp_dir().join("agentflare-browser-env-test-empty");
        std::fs::create_dir_all(&dir).unwrap();
        let json = serde_json::json!({"PATH": dir.to_string_lossy()}).to_string();
        assert!(parse_env_output(&json).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cached_backend_ignores_a_stale_or_missing_entry() {
        let cache = cached_backend_file();
        let saved = std::fs::read_to_string(&cache).ok();

        std::fs::remove_file(&cache).ok();
        assert!(cached_backend().is_none());

        std::fs::write(
            &cache,
            r#"{"bin":"/definitely/does/not/exist/agent-browser","path_env":"/x"}"#,
        )
        .unwrap();
        assert!(
            cached_backend().is_none(),
            "stale entries must not be trusted"
        );

        let dir = std::env::temp_dir().join("agentflare-browser-cache-test");
        std::fs::create_dir_all(&dir).unwrap();
        let real_bin = dir.join(flare_browser::BACKEND_BIN);
        std::fs::write(
            &real_bin,
            "#!/bin/sh
",
        )
        .unwrap();
        write_backend_cache(&real_bin, "/some/path");
        assert_eq!(cached_backend(), Some((real_bin, "/some/path".to_string())));
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
        std::fs::remove_file(cached_backend_file()).ok();
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
        assert!(
            err.contains("mise install github:vercel-labs/agent-browser"),
            "{err}"
        );
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
