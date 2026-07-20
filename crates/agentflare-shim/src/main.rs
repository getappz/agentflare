//! Compiled PATH shim for AI-agent shell sessions -- cross-platform, same
//! pattern as mise's `crates/mise-shim`: one small binary, copied/hardlinked
//! under many tool names (git, cargo, git.exe, cargo.exe, ...) into
//! `~/.agentflare/shims/`, prepended to PATH. Each copy reads its own
//! filename (argv[0] / `current_exe`) to learn which tool it stands in for,
//! then either routes the call through `lean-ctx -c <tool>` or execs the
//! real binary untouched. Bundled alongside the main `agentflare` binary at
//! release time, same as mise bundles `mise-shim`.
//!
//! On Windows this is the only mechanism that reaches agent tool calls at
//! all: Claude Code's PowerShell tool runs
//! `pwsh.exe -NoProfile -NonInteractive -Command "..."`, and `-NoProfile`
//! skips `$PROFILE` entirely, so anything gated behind a shell profile
//! (lean-ctx's own `shell-hook.ps1`, or a shell-function approach) never
//! loads. PATH resolution of bare command names inside the `-Command`
//! payload happens regardless of `-NoProfile`, so a shim directory on PATH
//! still gets hit. On Unix this plays the same role a `.bashenv` function
//! would -- same gate logic, just compiled instead of shell.
//!
//! Gate order: kill switches -> agent-env marker -> `.agentflare` project
//! walk-up (stopping at `$HOME`, since `~/.agentflare` is agentflare's own
//! app-data dir, not a project marker -- a false-positive bug found and
//! fixed on the bash-function prototype of this same idea). This is for AI
//! agent CLIs only: anything outside that double gate must resolve to the
//! real binary, unmodified, with negligible overhead, since a shim dir on
//! PATH affects every process that resolves through it, system-wide.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{exit, Command};

const KILL_SWITCHES: &[&str] = &["LEAN_CTX_DISABLED", "LEAN_CTX_NO_HOOK"];

const AGENT_ENV_VARS: &[&str] = &[
    "LEAN_CTX_AGENT",
    "CLAUDECODE",
    "CURSOR_AGENT",
    "CODEX_CLI_SESSION",
    "GEMINI_SESSION",
    "CODEBUDDY",
];

const PROJECT_MARKER: &str = ".agentflare";

fn is_set(name: &str) -> bool {
    env::var_os(name).is_some_and(|v| !v.is_empty())
}

fn any_set(names: &[&str]) -> bool {
    names.iter().any(|n| is_set(n))
}

/// Walk up from `start` looking for `.agentflare`, stopping at `home`
/// (exclusive) -- `~/.agentflare` is agentflare's own data dir, not a
/// project marker, and would otherwise false-positive on everything
/// under the user's home directory.
fn in_scoped_project(start: &Path, home: Option<&Path>) -> bool {
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

/// PATH with `shim_dir` removed, so neither our own real-binary lookup nor a
/// `lean-ctx -c` child process resolves back into this shim (self-recursion).
fn path_without_shim_dir(shim_dir: &Path) -> Option<OsString> {
    let path_var = env::var_os("PATH")?;
    env::join_paths(env::split_paths(&path_var).filter(|p| p != shim_dir)).ok()
}

fn trace(msg: &str) {
    if is_set("AGENTFLARE_SHIM_TRACE") {
        eprintln!("[flare-trace] {msg}");
    }
}

fn run_real(tool: &str, filtered_path: Option<&OsString>, args: &[OsString]) -> ! {
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

fn main() {
    let exe = match env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("agentflare-shim: failed to determine executable path: {e}");
            exit(1);
        }
    };
    let Some(tool) = exe.file_stem().and_then(|s| s.to_str()).map(str::to_string) else {
        eprintln!("agentflare-shim: failed to determine tool name from executable path");
        exit(1);
    };
    let shim_dir: PathBuf = exe.parent().map_or_else(PathBuf::new, Path::to_path_buf);
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let filtered_path = path_without_shim_dir(&shim_dir);

    if any_set(KILL_SWITCHES) || !any_set(AGENT_ENV_VARS) {
        run_real(&tool, filtered_path.as_ref(), &args);
    }

    let cwd = env::current_dir().unwrap_or_default();
    if !in_scoped_project(&cwd, dirs::home_dir().as_deref()) {
        run_real(&tool, filtered_path.as_ref(), &args);
    }

    trace(&format!("dispatch: lean-ctx -c {tool}"));
    let mut cmd = Command::new("lean-ctx");
    cmd.arg("-c").arg(&tool).args(&args);
    if let Some(p) = &filtered_path {
        cmd.env("PATH", p);
    }
    match cmd.status() {
        Ok(status) => {
            let code = status.code().unwrap_or(1);
            if code == 126 || code == 127 {
                run_real(&tool, filtered_path.as_ref(), &args);
            }
            exit(code);
        }
        Err(_) => run_real(&tool, filtered_path.as_ref(), &args),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn finds_marker_in_start_dir() {
        let tmp = std::env::temp_dir().join(format!("agentflare-shim-test-{}", std::process::id()));
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join(PROJECT_MARKER), "").unwrap();
        assert!(in_scoped_project(&tmp, None));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn finds_marker_in_an_ancestor_dir() {
        let tmp =
            std::env::temp_dir().join(format!("agentflare-shim-test-anc-{}", std::process::id()));
        let sub = tmp.join("a").join("b");
        fs::create_dir_all(&sub).unwrap();
        fs::write(tmp.join(PROJECT_MARKER), "").unwrap();
        assert!(in_scoped_project(&sub, None));
        let _ = fs::remove_dir_all(&tmp);
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
        fs::create_dir_all(&sub).unwrap();
        fs::write(tmp.join(PROJECT_MARKER), "").unwrap();
        assert!(!in_scoped_project(&sub, Some(&tmp)));
        let _ = fs::remove_dir_all(&tmp);
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
        fs::create_dir_all(&tmp).unwrap();
        assert!(!in_scoped_project(&tmp, tmp.parent()));
        let _ = fs::remove_dir_all(&tmp);
    }
}
