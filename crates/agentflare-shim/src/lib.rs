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

/// PATH with `shim_dir` removed, so a shim binary's own real-binary lookup
/// (and any child process it spawns) doesn't resolve back into itself --
/// also strips any cargo build-profile directory tree (see
/// `is_cargo_target_profile_dir`), which matters during development/
/// testing when multiple cargo-built binaries share one `target/debug`.
#[must_use]
pub fn path_without_shim_dir(shim_dir: &Path) -> Option<OsString> {
    let path_var = env::var_os("PATH")?;
    env::join_paths(
        env::split_paths(&path_var).filter(|p| p != shim_dir && !is_cargo_target_profile_dir(p)),
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
