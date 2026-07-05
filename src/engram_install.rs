// engram (github.com/Gentleman-Programming/engram) has no safe universal
// one-liner: the maintainer's own docs say prebuilt Windows binaries get
// AV-flagged as false positives and explicitly recommend `go install`
// (compiles locally, never flagged) or Homebrew on macOS/Linux instead.
// Only install through one of those two safe paths; otherwise print the
// documented manual options.
use std::process::{Command, Stdio};

pub fn has(cmd: &str) -> bool {
    let checker = if cfg!(windows) { "where" } else { "which" };
    Command::new(checker)
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn engram_installed() -> bool {
    Command::new("engram")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub enum InstallOutcome {
    Started(String),
    NoSafePath(String),
}

/// Runs synchronously (unlike the old JS's detached background spawn) since
/// `leanstack init` is an explicit, one-shot user command — the user is
/// already waiting on it to finish, there's no session-start timeout budget
/// to protect here.
pub fn install_and_setup(agent: &str) -> InstallOutcome {
    if has("go") {
        let cmd = format!(
            "go install github.com/Gentleman-Programming/engram/cmd/engram@latest && engram setup {agent}"
        );
        return run_shell(&cmd, "go install");
    }
    if !cfg!(windows) && has("brew") {
        let cmd = format!("brew install gentleman-programming/tap/engram && engram setup {agent}");
        return run_shell(&cmd, "brew");
    }
    InstallOutcome::NoSafePath(if cfg!(windows) {
        "engram: no safe auto-install path (no Go toolchain, and prebuilt Windows binaries are AV-flagged per the project's own docs). Install Go then re-run, or see github.com/Gentleman-Programming/engram/releases and accept the AV warning yourself.".to_string()
    } else {
        "engram: no Go or Homebrew found. Install one, or see github.com/Gentleman-Programming/engram/blob/main/docs/INSTALLATION.md".to_string()
    })
}

fn run_shell(cmd: &str, via: &str) -> InstallOutcome {
    let result = if cfg!(windows) {
        Command::new("cmd").args(["/c", cmd]).status()
    } else {
        Command::new("sh").args(["-c", cmd]).status()
    };
    match result {
        Ok(status) if status.success() => {
            InstallOutcome::Started(format!("engram installed and set up via {via}"))
        }
        Ok(status) => InstallOutcome::NoSafePath(format!(
            "engram install via {via} failed (exit {:?}) — see github.com/Gentleman-Programming/engram/blob/main/docs/INSTALLATION.md",
            status.code()
        )),
        Err(e) => InstallOutcome::NoSafePath(format!("engram install via {via} failed to start: {e}")),
    }
}
