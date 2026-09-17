//! Browser automation for AI coding agents — consolidated best-of.
//!
//! Feature sources (all Apache-2.0/MIT, see NOTICE-adjacent docs):
//! - `vercel-labs/agent-browser` (Apache-2.0): CLI-first UX, ref-based
//!   snapshots (`@e1`), sessions, batch mode, auth state, network/HAR,
//!   tabs/frames/dialogs, trace/record, skills. This crate's argv shapes
//!   mirror its CLI 1:1.
//! - `microsoft/playwright-mcp` (Apache-2.0): secrets redaction and
//!   read-only tool annotations — adopted as catalog metadata.
//! - `browserbase/stagehand` (MIT): `observe`/`extract` self-healing
//!   primitives — adopted as deterministic local helpers over snapshots
//!   (text filter + JS eval); the LLM reasoning step stays with the caller.
//!
//! v1 delegates execution to the `agent-browser` sidecar binary (Rust,
//! same stack as agentflare). Everything here except [`run_blocking`] is
//! pure logic and backend-agnostic, so a future native CDP driver can
//! reuse the session catalog/redaction core unchanged.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Sidecar binary v1 shells out to. Pure Rust (same stack as agentflare) —
/// installed via mise (`mise use -g agent-browser`, aqua prebuilt binaries),
/// never npm and never a from-source cargo build on first use.
pub const BACKEND_BIN: &str = "agent-browser";
/// Set to any non-empty value to disable first-use auto-install (air-gapped
/// machines: install manually and the error path names the exact commands).
pub const NO_AUTO_INSTALL_ENV: &str = "AGENTFLARE_BROWSER_NO_AUTO_INSTALL";
/// Explicit session override, above the derived worktree-scoped default.
pub const SESSION_ENV: &str = "AGENTFLARE_BROWSER_SESSION";
/// Default output cap — snapshots stay well under typical MCP response limits.
pub const MAX_OUTPUT_CHARS: usize = 12_000;
/// Per-stream cap on raw bytes captured from the sidecar child process
/// before the rest is drained and discarded -- generous headroom over
/// `MAX_OUTPUT_CHARS` so a normal snapshot is never itself truncated here;
/// only a runaway/page-controlled stream (e.g. `console`) hits it.
const STREAM_READ_CAP_BYTES: u64 = 1024 * 1024;

/// One consolidated browser verb.
pub struct ActionDef {
    pub name: &'static str,
    pub description: &'static str,
    /// Mirrors Playwright-MCP's read-only annotations: snapshot/get/read/
    /// observe never mutate page state; everything else may.
    pub read_only: bool,
}

/// Consolidated catalog — vercel/agent-browser core + observe/extract
/// (Stagehand-inspired, deterministic) + status/doctor (local-only).
pub const ACTIONS: &[ActionDef] = &[
    ActionDef {
        name: "open",
        description: "Launch browser and navigate to a URL (also: goto, navigate)",
        read_only: false,
    },
    ActionDef {
        name: "snapshot",
        description: "Accessibility tree with @e refs — the primary page read (compact, token-efficient)",
        read_only: true,
    },
    ActionDef {
        name: "observe",
        description: "Snapshot filtered to lines matching a query (deterministic Stagehand-style observe; no model call)",
        read_only: true,
    },
    ActionDef {
        name: "extract",
        description: "Run JS via eval and return raw output; the caller model structures it (deterministic Stagehand-style extract)",
        read_only: false,
    },
    ActionDef {
        name: "click",
        description: "Click a ref or selector (fails early when covered — dismiss coverer, re-snapshot, retry)",
        read_only: false,
    },
    ActionDef {
        name: "fill",
        description: "Clear and fill a field: fill <target> <text>",
        read_only: false,
    },
    ActionDef {
        name: "type",
        description: "Type into an element without clearing",
        read_only: false,
    },
    ActionDef {
        name: "press",
        description: "Press a key (Enter, Tab, Control+a)",
        read_only: false,
    },
    ActionDef {
        name: "hover",
        description: "Hover over an element",
        read_only: false,
    },
    ActionDef {
        name: "select",
        description: "Select dropdown option by value or label",
        read_only: false,
    },
    ActionDef {
        name: "check",
        description: "Check a checkbox",
        read_only: false,
    },
    ActionDef {
        name: "uncheck",
        description: "Uncheck a checkbox",
        read_only: false,
    },
    ActionDef {
        name: "back",
        description: "History back",
        read_only: false,
    },
    ActionDef {
        name: "forward",
        description: "History forward",
        read_only: false,
    },
    ActionDef {
        name: "reload",
        description: "Reload the page",
        read_only: false,
    },
    ActionDef {
        name: "get",
        description: "Read state: get <text|html|value|title|url|...> <target?>",
        read_only: true,
    },
    ActionDef {
        name: "read",
        description: "Agent-readable markdown for a URL, or the rendered active tab when omitted",
        read_only: true,
    },
    ActionDef {
        name: "screenshot",
        description: "Screenshot to a path (or temp dir when omitted)",
        read_only: true,
    },
    ActionDef {
        name: "pdf",
        description: "Save page as PDF",
        read_only: true,
    },
    ActionDef {
        name: "eval",
        description: "Run JavaScript in the page",
        read_only: false,
    },
    ActionDef {
        name: "wait",
        description: "Wait for selector, text, URL, JS condition, or milliseconds",
        read_only: true,
    },
    ActionDef {
        name: "cookies",
        description: "Cookie ops: cookies [set <name> <val> | clear]",
        read_only: false,
    },
    ActionDef {
        name: "storage",
        description: "localStorage/sessionStorage ops",
        read_only: false,
    },
    ActionDef {
        name: "network",
        description: "Route/mock/block requests, inspect tracked requests, HAR record",
        read_only: false,
    },
    ActionDef {
        name: "tabs",
        description: "List/switch/open/close tabs (stable t1,t2 ids + labels)",
        read_only: false,
    },
    ActionDef {
        name: "dialog",
        description: "Accept/dismiss blocking JS dialogs",
        read_only: false,
    },
    ActionDef {
        name: "console",
        description: "Page console messages",
        read_only: true,
    },
    ActionDef {
        name: "errors",
        description: "Uncaught page JS exceptions",
        read_only: true,
    },
    ActionDef {
        name: "batch",
        description: "Multiple quoted commands in one invocation (one turn, one daemon round-trip)",
        read_only: false,
    },
    ActionDef {
        name: "state",
        description: "Auth-state save/load/list (tokens live here — gitignore state files)",
        read_only: false,
    },
    ActionDef {
        name: "close",
        description: "Close the browser/session",
        read_only: false,
    },
    ActionDef {
        name: "doctor",
        description: "Diagnose the install (delegates to the sidecar's own doctor)",
        read_only: true,
    },
    ActionDef {
        name: "status",
        description: "Local-only: backend presence + resolved session, no browser launch",
        read_only: true,
    },
];

/// CLI spelling aliases mapped to real sidecar heads.
pub fn canonical_head(action: &str) -> &str {
    match action {
        "tabs" => "tab",
        "extract" => "eval",
        "goto" | "navigate" => "open",
        "quit" | "exit" => "close",
        other => other,
    }
}

/// Actions served locally without launching a browser.
pub fn is_local_only(action: &str) -> bool {
    matches!(action, "status")
}

pub fn find_action(name: &str) -> Option<&'static ActionDef> {
    ACTIONS.iter().find(|a| a.name == name)
}

/// Unknown verbs are treated as mutating (deny-by-default posture).
pub fn is_read_only(action: &str) -> bool {
    find_action(action).is_some_and(|a| a.read_only)
}

/// Stable per-directory session id (`af-` + 12 hex chars): concurrent
/// worktrees/agents each get an isolated browser session with zero config,
/// mirroring `--session <id>` + `session id --scope worktree` upstream.
pub fn default_session(cwd: &Path) -> String {
    let mut h = DefaultHasher::new();
    cwd.to_string_lossy().hash(&mut h);
    format!("af-{:012x}", h.finish() & 0xffff_ffff_ffff)
}

/// Explicit arg > `AGENTFLARE_BROWSER_SESSION` env > derived default.
pub fn resolve_session(explicit: Option<&str>, cwd: &Path) -> String {
    if let Some(s) = explicit.filter(|s| !s.trim().is_empty()) {
        return s.trim().to_string();
    }
    if let Ok(s) = std::env::var(SESSION_ENV)
        && !s.trim().is_empty()
    {
        return s.trim().to_string();
    }
    default_session(cwd)
}

/// Locate the sidecar: `PATH` first, then the cargo bin dir (`$CARGO_HOME`
/// or `~/.cargo/bin`, plus `%USERPROFILE%\.cargo\bin` on Windows) so a
/// `cargo install` that didn't touch `PATH` still resolves. Skips mise's
/// shims dir (see [`is_mise_shims_dir`]) so a dead shim there never shadows
/// the absolute path `mise which` resolves in `browser_install`.
pub fn find_backend() -> Result<PathBuf, String> {
    let paths = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&paths) {
        if is_mise_shims_dir(&dir) {
            continue;
        }
        if let Some(p) = check_bin_dir(&dir) {
            return Ok(p);
        }
    }
    if let Some(p) = cargo_bin_candidates().into_iter().find(|p| p.is_file()) {
        return Ok(p);
    }
    Err(missing_hint())
}

/// `~/.local/share/mise/shims` (mirrors the same hardcoded path
/// `daemon_autostart::daemon_path_env` appends for mise-managed agent
/// CLIs). mise generates a shim for every tool it has ever installed,
/// regardless of activation — but the git backend here is deliberately
/// `mise install`ed only, never `mise use`d (see `browser_install`'s module
/// doc: "shims never need activation"), so its shim has no active version
/// to resolve and errors when invoked directly. A naive PATH scan would
/// still find that dead shim file and return it as if it were the real
/// binary, so it must be skipped explicitly.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn is_mise_shims_dir(dir: &Path) -> bool {
    std::env::var_os("HOME").is_some_and(|home| {
        dir == PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("mise")
            .join("shims")
    })
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn is_mise_shims_dir(_dir: &Path) -> bool {
    false
}

/// Install hint — mise's git backend only (prebuilt binary, no toolchain).
/// The actual install runs from `browser_install::ensure_agent_browser`,
/// which also bootstraps mise itself when absent.
pub fn missing_hint() -> String {
    format!(
        "{BACKEND_BIN} not found — install it from the git repo via mise (prebuilt binary, no toolchain): `mise install github:vercel-labs/agent-browser@latest && agent-browser install` (fetches Chrome for Testing on first `install`). First use auto-installs this for you unless {NO_AUTO_INSTALL_ENV} is set."
    )
}

/// False only when `{NO_AUTO_INSTALL_ENV}` is set non-empty.
pub fn auto_install_enabled() -> bool {
    std::env::var_os(NO_AUTO_INSTALL_ENV).is_none_or(|v| v.is_empty())
}

fn check_bin_dir(dir: &Path) -> Option<PathBuf> {
    let cand = dir.join(BACKEND_BIN);
    if cand.is_file() {
        return Some(cand);
    }
    #[cfg(windows)]
    {
        let exe = dir.join(format!("{BACKEND_BIN}.exe"));
        if exe.is_file() {
            return Some(exe);
        }
    }
    None
}

fn cargo_bin_candidates() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        roots.push(PathBuf::from(home));
    }
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".cargo"));
    }
    #[cfg(windows)]
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        roots.push(PathBuf::from(profile).join(".cargo"));
    }
    roots
        .into_iter()
        .map(|r| r.join("bin"))
        .filter_map(|d| check_bin_dir(&d))
        .collect()
}

/// Global sidecar flags prepended to every invocation.
pub fn global_args(session: &str) -> Vec<String> {
    vec!["--session".to_string(), session.to_string()]
}

/// Build full sidecar argv for `action` (which may carry a subcommand,
/// e.g. `"get text"` or `"tab new"`) plus positional/extra args.
/// Validates the head verb against [`ACTIONS`] (after alias mapping).
pub fn build_argv(
    session: &str,
    action: &str,
    positionals: &[String],
    extra: &[String],
) -> Result<Vec<String>, String> {
    let mut words: Vec<&str> = action.split_whitespace().collect();
    if words.is_empty() {
        return Err("browser action is required".to_string());
    }
    let head = canonical_head(words[0]);
    // Validate both spellings: a literal catalog name (e.g. "tabs", which
    // itself maps to the sidecar head "tab") and a pure alias (e.g. "goto",
    // which isn't in ACTIONS at all but canonicalizes to "open").
    if !is_local_only(head) && find_action(head).is_none() && find_action(words[0]).is_none() {
        let valid: Vec<&str> = ACTIONS.iter().map(|a| a.name).collect();
        return Err(format!(
            "unknown browser action: {action} (valid: {})",
            valid.join(", ")
        ));
    }
    words[0] = head;
    let mut argv = global_args(session);
    argv.extend(words.into_iter().map(str::to_string));
    argv.extend_from_slice(positionals);
    argv.extend_from_slice(extra);
    Ok(argv)
}

/// Run the sidecar synchronously; on success returns trimmed stdout, on
/// failure a one-line summary plus a bounded stderr excerpt (snapshots can
/// be large — never dump them raw into an error path). `secrets` is
/// redacted from the stderr excerpt before truncation, matching the
/// success-path caller's own redact-before-compact order. `path_env`, when
/// set, is the `PATH` `browser_install::ensure_agent_browser` resolved via
/// `mise env --json` for a mise-installed backend — merged into the child's
/// env so it sees the same `PATH` mise would activate for it.
pub fn run_blocking(
    program: &Path,
    args: &[String],
    secrets: &[String],
    path_env: Option<&str>,
) -> Result<String, String> {
    // flare_process::command (not std::process::Command::new) so a daemon or
    // IDE-launched MCP server with no inherited console never flashes one
    // over the user's desktop when it spawns the sidecar (Windows).
    let mut cmd = flare_process::command(program);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(path) = path_env {
        cmd.env("PATH", path);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn {}: {e}", program.display()))?;
    let mut stdout_pipe = child.stdout.take().expect("stdout piped above");
    let mut stderr_pipe = child.stderr.take().expect("stderr piped above");
    // Drain both pipes concurrently and capped: `Command::output()` buffers
    // an unbounded amount before we ever see it, which a runaway or
    // page-controlled stream (e.g. `console`) can use to exhaust memory.
    // Reading the two pipes one at a time instead of concurrently risks
    // deadlock — the child blocks writing to whichever pipe fills its OS
    // buffer first while we're still draining the other.
    let (stdout_buf, stderr_buf) = std::thread::scope(|scope| {
        let stdout_job = scope.spawn(|| read_capped(&mut stdout_pipe, STREAM_READ_CAP_BYTES));
        let stderr_buf = read_capped(&mut stderr_pipe, STREAM_READ_CAP_BYTES);
        (stdout_job.join().unwrap_or_default(), stderr_buf)
    });
    let status = child
        .wait()
        .map_err(|e| format!("failed to wait on {}: {e}", program.display()))?;
    let stdout = String::from_utf8_lossy(&stdout_buf).trim().to_string();
    if status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&stderr_buf).trim().to_string();
    let excerpt = compact_output(&redact(&stderr, secrets), 2000);
    let code = status
        .code()
        .map_or("signal".to_string(), |c| c.to_string());
    Err(if excerpt.is_empty() {
        format!("{BACKEND_BIN} exited with status {code} (no stderr)")
    } else {
        format!("{BACKEND_BIN} exited with status {code}: {excerpt}")
    })
}

/// Reads up to `cap` bytes from `pipe`, then drains (and discards) any
/// remainder without buffering it — keeps memory bounded on a runaway
/// stream while still letting the child finish writing instead of
/// blocking forever on a full OS pipe buffer.
fn read_capped(pipe: &mut impl Read, cap: u64) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = std::io::copy(&mut pipe.by_ref().take(cap), &mut buf);
    let _ = std::io::copy(pipe, &mut std::io::sink());
    buf
}

/// Hard-cap output length with an explicit truncation marker (keeps MCP
/// responses and terminal output bounded; snapshots are re-runnable).
pub fn compact_output(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_string();
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}… [truncated: showing {limit} of {} chars — re-run with a narrower snapshot/observe query]",
        &text[..end],
        text.len()
    )
}

/// Playwright-MCP-style secrets hygiene: caller-supplied secret values are
/// scrubbed from output before it reaches the model. Convenience, not a
/// security boundary — same caveat as upstream.
pub fn redact(text: &str, secrets: &[String]) -> String {
    let mut out = text.to_string();
    for s in secrets.iter().filter(|s| !s.is_empty()) {
        out = out.replace(s.as_str(), "***");
    }
    out
}

/// Deterministic Stagehand-style `observe`: keep snapshot lines matching
/// the query (case-insensitive substring), cap at `limit` lines.
pub fn observe_filter(snapshot: &str, query: &str, limit: usize) -> String {
    let q = query.to_lowercase();
    let hits: Vec<&str> = snapshot
        .lines()
        .filter(|l| l.to_lowercase().contains(&q))
        .take(limit)
        .collect();
    if hits.is_empty() {
        return format!("observe: no snapshot lines match {query:?}");
    }
    format!(
        "observe: {} match(es) for {query:?}\n{}",
        hits.len(),
        hits.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_session_is_stable_and_scoped() {
        let a = default_session(Path::new("/repo/worktree-a"));
        let b = default_session(Path::new("/repo/worktree-a"));
        let c = default_session(Path::new("/repo/worktree-b"));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("af-"));
    }

    #[test]
    fn resolve_session_prefers_explicit_then_env() {
        let cwd = Path::new("/x");
        assert_eq!(resolve_session(Some("s1"), cwd), "s1");
        // SAFETY: single-threaded test mutating process env for itself.
        unsafe { std::env::set_var(SESSION_ENV, "from-env") };
        assert_eq!(resolve_session(None, cwd), "from-env");
        assert_eq!(resolve_session(Some("  "), cwd), "from-env");
        unsafe { std::env::remove_var(SESSION_ENV) };
        assert_eq!(resolve_session(None, cwd), default_session(cwd));
    }

    #[test]
    fn build_argv_validates_and_aliases() {
        let argv = build_argv("s", "snapshot", &[], &[]).unwrap();
        assert_eq!(argv, vec!["--session", "s", "snapshot"]);
        // Subcommand passthrough + alias mapping.
        let argv = build_argv("s", "get text", &[String::from("@e1")], &[]).unwrap();
        assert_eq!(argv, vec!["--session", "s", "get", "text", "@e1"]);
        let argv = build_argv("s", "tabs", &[], &[]).unwrap();
        assert_eq!(argv, vec!["--session", "s", "tab"]);
        let argv = build_argv("s", "extract", &[String::from("1+1")], &[]).unwrap();
        assert_eq!(argv, vec!["--session", "s", "eval", "1+1"]);
        assert!(build_argv("s", "bogus", &[], &[]).is_err());
        assert!(build_argv("s", "   ", &[], &[]).is_err());
    }

    #[test]
    fn build_argv_accepts_pure_aliases_not_in_the_catalog() {
        // "goto"/"navigate"/"quit"/"exit" are canonical_head-only aliases —
        // they never appear in ACTIONS, so validation must key off the
        // canonicalized head, not the raw input word.
        let argv = build_argv("s", "goto", &[String::from("https://x")], &[]).unwrap();
        assert_eq!(argv, vec!["--session", "s", "open", "https://x"]);
        let argv = build_argv("s", "navigate", &[String::from("https://x")], &[]).unwrap();
        assert_eq!(argv, vec!["--session", "s", "open", "https://x"]);
        let argv = build_argv("s", "quit", &[], &[]).unwrap();
        assert_eq!(argv, vec!["--session", "s", "close"]);
        let argv = build_argv("s", "exit", &[], &[]).unwrap();
        assert_eq!(argv, vec!["--session", "s", "close"]);
    }

    #[test]
    fn read_only_posture_is_deny_by_default() {
        assert!(is_read_only("snapshot"));
        assert!(is_read_only("observe"));
        assert!(is_read_only("read"));
        assert!(!is_read_only("click"));
        assert!(!is_read_only("bogus-action"));
        // "extract" canonicalizes to and executes as "eval" (unrestricted JS,
        // read_only: false) -- it must never report itself read-only, or a
        // client/gate trusting this flag would auto-approve a mutating call.
        assert!(!is_read_only("extract"));
    }

    #[test]
    fn compact_and_redact_behave() {
        assert_eq!(compact_output("hi", 10), "hi");
        let big = "x".repeat(100);
        let out = compact_output(&big, 10);
        assert!(out.contains("[truncated:"));
        assert_eq!(
            redact("token abc123 here", &["abc123".to_string(), String::new()]),
            "token *** here"
        );
    }

    #[test]
    fn observe_filter_matches_case_insensitively() {
        let snap = "- heading \"Hi\" [ref=e1]\n- link \"More\" [ref=e2]";
        let out = observe_filter(snap, "more", 10);
        assert!(out.contains("@e2") || out.contains("e2"));
        assert!(!out.contains("e1"));
        assert!(observe_filter(snap, "zzz", 10).contains("no snapshot lines match"));
    }

    #[test]
    fn install_hint_is_mise_only() {
        let hint = missing_hint();
        assert!(
            hint.contains("mise install github:vercel-labs/agent-browser"),
            "{hint}"
        );
        assert!(!hint.contains("npm"), "{hint}");
        assert!(!hint.contains("npx"), "{hint}");
        assert!(!hint.contains("cargo install"), "{hint}");
        assert!(!hint.contains("mise use -g"), "{hint}");
    }

    #[test]
    fn auto_install_defaults_on_and_env_disables() {
        // SAFETY: single-threaded env fiddling, restored before return.
        let saved = std::env::var_os(NO_AUTO_INSTALL_ENV);
        unsafe { std::env::remove_var(NO_AUTO_INSTALL_ENV) };
        assert!(auto_install_enabled());
        unsafe { std::env::set_var(NO_AUTO_INSTALL_ENV, "1") };
        assert!(!auto_install_enabled());
        unsafe { std::env::remove_var(NO_AUTO_INSTALL_ENV) };
        if let Some(v) = saved {
            unsafe { std::env::set_var(NO_AUTO_INSTALL_ENV, v) };
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn is_mise_shims_dir_matches_only_the_real_shims_path() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // SAFETY: single-threaded env fiddling, restored before return.
        let saved_home = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", home) };

        assert!(is_mise_shims_dir(
            &home.join(".local").join("share").join("mise").join("shims")
        ));
        assert!(!is_mise_shims_dir(&home.join(".cargo").join("bin")));
        assert!(!is_mise_shims_dir(
            &home.join(".local").join("share").join("mise")
        ));

        match saved_home {
            Some(v) => unsafe { std::env::set_var("HOME", v) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn find_backend_skips_a_dead_mise_shim_on_path() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let shims = home.join(".local").join("share").join("mise").join("shims");
        std::fs::create_dir_all(&shims).unwrap();
        std::fs::write(shims.join(BACKEND_BIN), "#!/bin/sh\nexit 1\n").unwrap();

        // SAFETY: single-threaded env fiddling (nextest isolates each test
        // in its own process), restored before return.
        let saved_home = std::env::var_os("HOME");
        let saved_path = std::env::var_os("PATH");
        let saved_cargo = std::env::var_os("CARGO_HOME");
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("PATH", &shims);
            std::env::remove_var("CARGO_HOME");
        }

        let err = find_backend().unwrap_err();
        assert!(err.contains(BACKEND_BIN), "{err}");

        unsafe {
            match saved_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
            match saved_path {
                Some(v) => std::env::set_var("PATH", v),
                None => std::env::remove_var("PATH"),
            }
            match saved_cargo {
                Some(v) => std::env::set_var("CARGO_HOME", v),
                None => std::env::remove_var("CARGO_HOME"),
            }
        }
    }
}
