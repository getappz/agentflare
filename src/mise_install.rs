// Cross-platform detect-or-install for mise (github.com/jdx/mise), a dev-tool
// version manager. agentflare uses it as a uniform, host-independent way to
// provide the external toolchains its integrations need — Go for the engram
// binary, Node/npm for lean-ctx — on machines that don't already have them.
//
// This module only handles mise itself (detection + bootstrap). Installing the
// individual tools through mise happens at each tool's install site.
use crate::paths::home;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub enum MiseOutcome {
    /// Already on the system (path to the binary).
    Present(String),
    /// We just installed it (path to the binary).
    Installed(String),
    /// Not present and could not be installed (reason).
    Failed(String),
}

/// A usable mise binary, or `None`. Checks PATH first, then mise's default
/// per-OS install location — a freshly-installed mise lands outside the
/// current process's PATH, so "just installed" wouldn't otherwise be visible
/// until a new shell.
pub fn mise_bin() -> Option<String> {
    if let Some(p) = which("mise") {
        return Some(p);
    }
    default_locations()
        .into_iter()
        .find(|p| p.exists())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Ensure mise is available, installing it cross-platform if absent.
pub fn ensure_mise() -> MiseOutcome {
    if let Some(bin) = mise_bin() {
        return MiseOutcome::Present(bin);
    }
    if let Err(e) = install() {
        return MiseOutcome::Failed(e);
    }
    match mise_bin() {
        Some(bin) => MiseOutcome::Installed(bin),
        None => MiseOutcome::Failed(
            "mise installer reported success but the binary was not found on PATH \
             or its default install location — open a new shell and re-run, or see \
             https://mise.jdx.dev/installing-mise.html"
                .to_string(),
        ),
    }
}

/// mise's default install location per OS (see mise's own install docs):
/// `~/.local/bin/mise` on Unix, `%LOCALAPPDATA%\mise\bin\mise.exe` on Windows.
fn default_locations() -> Vec<PathBuf> {
    if cfg!(windows) {
        let local = std::env::var("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| home().join("AppData").join("Local"));
        vec![local.join("mise").join("bin").join("mise.exe")]
    } else {
        vec![home().join(".local").join("bin").join("mise")]
    }
}

fn install() -> Result<(), String> {
    if cfg!(windows) {
        install_windows()
    } else {
        install_unix()
    }
}

/// Official installer (https://mise.run) via curl, wget as a fallback for
/// curl-less minimal images. Installs to ~/.local/bin/mise.
fn install_unix() -> Result<(), String> {
    let cmd = if has("curl") {
        "curl -fsSL https://mise.run | sh"
    } else if has("wget") {
        "wget -qO- https://mise.run | sh"
    } else {
        return Err(
            "cannot install mise: neither curl nor wget is available. Install one, \
             or install mise manually per https://mise.jdx.dev/installing-mise.html"
                .to_string(),
        );
    };
    run_shell(cmd)
}

/// Windows has no official one-line install *script* (the PowerShell snippet in
/// mise's docs only wires activation), so use the package managers mise itself
/// recommends: winget, then scoop.
fn install_windows() -> Result<(), String> {
    if has("winget")
        && run_status(
            "winget",
            &[
                "install",
                "-e",
                "--id",
                "jdx.mise",
                "--silent",
                "--accept-source-agreements",
                "--accept-package-agreements",
            ],
        )
    {
        return Ok(());
    }
    if has("scoop") && run_status("scoop", &["install", "mise"]) {
        return Ok(());
    }
    Err(
        "cannot install mise on Windows: neither winget nor scoop succeeded. Install \
         one (or mise directly) per https://mise.jdx.dev/installing-mise.html"
            .to_string(),
    )
}

/// `mise use -g <spec>` — install/activate a tool globally. Run from $HOME so
/// an untrusted project-local mise config in the user's cwd can't block a
/// global install.
pub fn use_global(mise: &str, spec: &str) -> bool {
    Command::new(mise)
        .current_dir(home())
        .args(["use", "-g", spec])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Install a tool globally from a backend spec (e.g.
/// `github:Gentleman-Programming/engram`) and return the absolute path mise
/// resolves `bin` to. mise installs into its own data dir, which is NOT on
/// PATH — so callers use this absolute path directly (as an MCP command, etc.)
/// rather than expecting the bare name to resolve. That's the whole point of
/// routing through mise: a PATH-independent, cross-platform install path.
pub fn install_tool(mise: &str, spec: &str, bin: &str) -> Result<String, String> {
    if !use_global(mise, spec) {
        return Err(format!("`mise use -g {spec}` failed"));
    }
    which_tool(mise, bin)
        .ok_or_else(|| format!("mise installed {spec} but `mise which {bin}` returned no path"))
}

/// Absolute path mise resolves `bin` to, or `None`. Reads stdout only — mise
/// can emit unrelated warnings on stderr.
pub fn which_tool(mise: &str, bin: &str) -> Option<String> {
    let out = Command::new(mise)
        .current_dir(home())
        .args(["which", bin])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .last()
        .map(|l| l.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn which(cmd: &str) -> Option<String> {
    let checker = if cfg!(windows) { "where" } else { "which" };
    let out = Command::new(checker)
        .arg(cmd)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

fn has(cmd: &str) -> bool {
    which(cmd).is_some()
}

fn run_shell(cmd: &str) -> Result<(), String> {
    let result = if cfg!(windows) {
        Command::new("cmd").args(["/c", cmd]).status()
    } else {
        Command::new("sh").args(["-c", cmd]).status()
    };
    match result {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("mise installer exited with {:?}", s.code())),
        Err(e) => Err(format!("failed to run mise installer: {e}")),
    }
}

fn run_status(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_locations_are_platform_appropriate_and_nonempty() {
        let locs = default_locations();
        assert!(!locs.is_empty());
        let joined = locs.iter().map(|p| p.to_string_lossy()).collect::<String>();
        assert!(joined.contains("mise"));
        if cfg!(windows) {
            assert!(joined.ends_with("mise.exe"));
        }
    }

    #[test]
    fn which_returns_none_for_a_nonexistent_command() {
        assert!(which("definitely-not-a-real-binary-xyz-123").is_none());
    }
}
