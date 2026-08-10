//! On-demand installer for bubblewrap, sourced from openai/codex's own
//! prebuilt, statically-linked release binaries via `mise`'s `github`
//! backend -- bubblewrap's own GitHub releases ship a source tarball only,
//! so there is nothing for a binary installer to grab from upstream
//! directly. Verified against a real WSL2 install: `mise install
//! "github:openai/codex[matching_regex=^bwrap-.*\.tar\.gz$]@rust-v0.147.0"`
//! correctly disambiguates the right binary out of a release that also
//! bundles unrelated tools (codex itself, argument-comment-lint, ...), for
//! the right host arch, and the extracted binary runs.
//!
//! Requires `mise` on `PATH` -- agentflare already depends on it for agent
//! launches (see `mise_install.rs`), so this is an existing dependency at a
//! new call site, not a new one.
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CODEX_TAG: &str = "rust-v0.147.0";
const MISE_TOOL: &str = "github:openai/codex";
const MISE_TOOL_SPEC: &str = "github:openai/codex[matching_regex=^bwrap-.*\\.tar\\.gz$]";

/// Upper bound on how long `mise install` may run before it's killed. This
/// runs synchronously on a job-runner worker thread *before* that job's own
/// timeout accounting starts, so an install that hangs (stalled network,
/// `mise` waiting on a prompt) would otherwise stall the worker forever.
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Installs bwrap via `mise install` and returns the installed binary's
/// path, or `None` if `mise` isn't on `PATH` or the install failed.
pub(super) fn install() -> Option<PathBuf> {
    let mise = which("mise")?;
    let spec = format!("{MISE_TOOL_SPEC}@{CODEX_TAG}");
    let child = Command::new(&mise)
        .args(["install", &spec])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .ok()?;
    if !wait_with_timeout(child, INSTALL_TIMEOUT) {
        return None;
    }

    let output = Command::new(&mise)
        .args(["where", &format!("{MISE_TOOL}@{CODEX_TAG}")])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let install_dir = String::from_utf8(output.stdout).ok()?;
    let bin = PathBuf::from(install_dir.trim()).join("bwrap");
    super::is_executable(&bin).then_some(bin)
}

/// Waits for `child` to exit, killing it if `timeout` elapses first. Returns
/// whether it exited successfully (`false` on failure, timeout, or a wait
/// error).
fn wait_with_timeout(mut child: Child, timeout: Duration) -> bool {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return false,
        }
    }
}

fn which(cmd: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(cmd))
        .find(|candidate| super::is_executable(candidate))
}
