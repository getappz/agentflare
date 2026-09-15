// First-use installer for the agent-browser sidecar via mise.
//
// Source: the mise registry shorthand `agent-browser`, which resolves to the
// aqua backend — prebuilt binaries pulled from the vercel-labs/agent-browser
// GitHub releases (linux/macOS/Windows, no toolchain, no compile, no npm).
// mise itself is bootstrapped through `mise_install` (curl|sh on unix,
// winget/scoop on windows) when absent, so this chain has no cargo, node,
// or brew prerequisite anywhere.
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Registry shorthand + floating version: aqua prebuilt binary, latest release.
const MISE_SPEC: &str = "agent-browser@latest";
/// Bound for stderr tails quoted in install errors (full logs stay on the
/// caller side; MCP responses must stay compact).
const ERR_TAIL_CHARS: usize = 2000;

/// Ensure the sidecar exists, auto-installing on first use when enabled.
/// Resolution order: `PATH`/cargo-bin hit (free) → `mise use -g` (one-time
/// download) → `agent-browser install` (Chrome for Testing fetch).
/// Concurrent first-use callers serialize on a temp-dir lock; losers
/// re-check and reuse the winner's install. Returns the binary path —
/// always absolute (via `mise which`), so mise shims never need activation.
pub fn ensure_agent_browser(auto_install: bool) -> Result<PathBuf, String> {
    if let Ok(p) = flare_browser::find_backend() {
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
    let use_out = Command::new(&mise)
        .args(["use", "-g", MISE_SPEC])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to spawn `{mise} use`: {e}"))?;
    if !use_out.status.success() {
        return Err(format!(
            "`mise use -g {MISE_SPEC}` failed (exit {:?}): {}",
            use_out.status.code(),
            tail(&String::from_utf8_lossy(&use_out.stderr)),
        ));
    }
    // Absolute path out (shims require shell activation; this must work from
    // bare subprocess spawns in CLI and MCP alike).
    let bin = resolve_via_mise_which(&mise)?;
    let chrome_out = Command::new(&bin)
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
    Ok(bin)
}

fn resolve_via_mise_which(mise: &str) -> Result<PathBuf, String> {
    let out = Command::new(mise)
        .args(["which", "agent-browser"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("failed to spawn `{mise} which`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`mise which agent-browser` failed after a successful install (exit {:?}): {} — retry manually: `mise use -g {MISE_SPEC}`",
            out.status.code(),
            tail(&String::from_utf8_lossy(&out.stderr)),
        ));
    }
    parse_which_output(&String::from_utf8_lossy(&out.stdout))
}

fn parse_which_output(stdout: &str) -> Result<PathBuf, String> {
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or_else(|| {
            "`mise which agent-browser` printed no path — retry manually: `mise use -g agent-browser@latest`"
                .to_string()
        })?;
    let path = PathBuf::from(line);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "`mise which agent-browser` resolved to {line}, which is not a file — retry manually: `mise use -g agent-browser@latest`"
        ))
    }
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
    let file =
        std::fs::File::create(&path).map_err(|e| format!("lock file {}: {e}", path.display()))?;
    file.lock_exclusive()
        .map_err(|e| format!("install lock {}: {e}", path.display()))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_which_accepts_first_nonblank_line_pointing_at_a_file() {
        let dir = std::env::temp_dir();
        let file = dir.join("agentflare-browser-which-test-bin");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();
        let out = format!("\n  \n{}\n", file.display());
        assert_eq!(parse_which_output(&out).unwrap(), file);
        std::fs::remove_file(&file).ok();
    }

    #[test]
    fn parse_which_rejects_empty_and_missing_paths() {
        assert!(parse_which_output("   \n").is_err());
        let missing = std::env::temp_dir().join("agentflare-browser-which-test-missing-xyz");
        assert!(parse_which_output(&missing.to_string_lossy()).is_err());
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
        assert!(err.contains("mise use -g agent-browser"), "{err}");
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
