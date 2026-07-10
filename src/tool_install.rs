// Native installers for external CLI tools, declared as data. Each tool lists
// its official install methods in priority order; the runner detects which
// required helper (curl, brew, …) is present on this platform and runs the
// first method that fits. No version manager needed — the tools' own installers
// fetch prebuilt binaries, verify checksums, and fix PATH themselves.
//
// This is the counterpart to engram's mise path: engram has no universal
// installer script (only go/brew/prebuilt), so it routes through mise; tools
// that DO ship a proper `curl | sh` installer use it directly here.
use std::process::{Command, Stdio};

/// One official install method for a tool.
pub struct Method {
    /// Helper that must be on PATH to use this method (e.g. "curl", "brew").
    pub requires: &'static str,
    /// Self-contained shell command that installs — and, where the upstream
    /// installer doesn't do it itself, onboards — the tool.
    pub command: &'static str,
}

/// An installable external tool and its native install methods.
pub struct Tool {
    pub id: &'static str,
    /// Binary name, used to detect an existing install.
    pub bin: &'static str,
    /// Install methods, highest priority first.
    pub methods: &'static [Method],
    /// Best-effort commands run (as direct process spawns — no shell) after a
    /// successful install; each entry is a full argv. For lean-ctx this
    /// allowlists `agentflare` in lean-ctx's own shell hook: once onboarded,
    /// that hook blocks any non-allowlisted command — including agentflare's own
    /// re-invocations (its SessionStart hooks, its MCP server) — so agentflare
    /// must add itself, or it locks itself out of every hooked shell.
    pub post_install: &'static [&'static [&'static str]],
}

/// lean-ctx (github.com/yvgude/lean-ctx) — token-efficient context tool.
///
/// The universal installer downloads a prebuilt binary from GitHub releases,
/// SHA256-verifies it, installs to ~/.local/bin, fixes PATH, and runs `onboard`
/// itself — so `curl | sh` is the whole install. We fetch the script from
/// GitHub raw rather than leanctx.com, which fronts the same script but is
/// unreliable. brew is the platform-native alternative; its formula doesn't
/// onboard, so that command does it explicitly.
pub const LEAN_CTX: Tool = Tool {
    id: "lean-ctx",
    bin: "lean-ctx",
    methods: &[
        Method {
            requires: "curl",
            command: "curl -fsSL https://raw.githubusercontent.com/yvgude/lean-ctx/main/install.sh | sh",
        },
        Method {
            requires: "brew",
            command: "brew tap yvgude/lean-ctx && brew install lean-ctx && lean-ctx onboard",
        },
    ],
    // lean-ctx onboard installs a shell hook that gates non-allowlisted
    // commands. Allow agentflare (so it can run itself) and mise (agentflare
    // installs engram through it and `agentflare run` launches via it — mise is
    // not in lean-ctx's built-in default allowlist). Run as a direct process
    // spawn (not `sh -c`), because that same hook also blocks `sh -c` inline
    // execution outright — an allowlist entry wouldn't save it.
    post_install: &[&["lean-ctx", "allow", "agentflare", "mise"]],
};

/// Whether `tool` is already installed (its binary resolves on PATH).
pub fn installed(tool: &Tool) -> bool {
    has(tool.bin)
}

/// Install `tool` via the first method whose required helper is present.
/// Returns a human-readable status, or an error naming the helpers it needs.
pub fn install(tool: &Tool) -> Result<String, String> {
    if cfg!(windows) {
        return Err(format!(
            "{}: no native installer for Windows yet — build from source",
            tool.id
        ));
    }
    let Some(method) = tool.methods.iter().find(|m| has(m.requires)) else {
        let helpers: Vec<_> = tool.methods.iter().map(|m| m.requires).collect();
        return Err(format!(
            "{}: no native installer available — install one of [{}] first",
            tool.id,
            helpers.join(", ")
        ));
    };
    match run_shell(method.command) {
        Ok(()) => {
            // Best-effort — a failed post-step (e.g. allowlisting) shouldn't
            // undo a successful install.
            for argv in tool.post_install {
                let _ = run_direct(argv);
            }
            Ok(format!("{} installed via {}", tool.id, method.requires))
        }
        Err(e) => Err(format!(
            "{} install via {} failed — {e}. Run manually: {}",
            tool.id, method.requires, method.command
        )),
    }
}

fn has(cmd: &str) -> bool {
    let checker = if cfg!(windows) { "where" } else { "which" };
    Command::new(checker)
        .arg(cmd)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Spawn a command directly (no shell), with ~/.local/bin prepended to PATH so
/// a binary the native installer just dropped there resolves. Direct spawn also
/// dodges lean-ctx's `sh -c` block. Returns whether it succeeded.
fn run_direct(argv: &[&str]) -> bool {
    let Some((prog, args)) = argv.split_first() else {
        return false;
    };
    let existing = std::env::var_os("PATH").unwrap_or_default();
    let mut dirs = vec![crate::paths::home().join(".local").join("bin")];
    dirs.extend(std::env::split_paths(&existing));
    let path = std::env::join_paths(dirs).unwrap_or(existing);
    Command::new(prog)
        .args(args)
        .env("PATH", path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_shell(cmd: &str) -> Result<(), String> {
    let result = if cfg!(windows) {
        Command::new("cmd").args(["/c", cmd]).status()
    } else {
        Command::new("sh").args(["-c", cmd]).status()
    };
    match result {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("exit {:?}", s.code())),
        Err(e) => Err(format!("failed to start: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lean_ctx_is_declared_with_ordered_nonempty_methods() {
        assert_eq!(LEAN_CTX.bin, "lean-ctx");
        assert!(!LEAN_CTX.methods.is_empty());
        // curl (universal, no extra deps) is preferred over brew.
        assert_eq!(LEAN_CTX.methods[0].requires, "curl");
        assert!(LEAN_CTX.methods.iter().all(|m| !m.command.is_empty()));
        // agentflare must allowlist itself (and mise) in lean-ctx's shell hook.
        assert!(LEAN_CTX.post_install.iter().any(|argv| {
            argv.first() == Some(&"lean-ctx")
                && argv.contains(&"agentflare")
                && argv.contains(&"mise")
        }));
    }

    #[test]
    fn install_reports_missing_helpers_when_none_present() {
        const FAKE: Tool = Tool {
            id: "fake",
            bin: "fake",
            methods: &[Method {
                requires: "definitely-not-a-real-helper-xyz-123",
                command: "true",
            }],
            post_install: &[],
        };
        let err = install(&FAKE).unwrap_err();
        // Both the missing-helper and windows branches say "no native installer".
        assert!(err.contains("no native installer"), "got: {err}");
    }
}
