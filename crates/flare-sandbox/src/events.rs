//! Structured sandbox events, mirroring OpenShell's OCSF audit findings in
//! miniature: every fallback-to-unsandboxed, skipped mount, identity
//! mismatch, and invalid config emits one payload-free line on stderr so a
//! job silently losing its boundary never looks identical to a sandboxed one.
//!
//! Events carry only destination, policy, and reason -- never credentials,
//! file contents, or prompt text (same rule as OpenShell's denial events).

use std::fmt::Write as _;

/// One sandbox lifecycle event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxEvent {
    /// Machine-readable kind: `fallback_unsandboxed`, `skipped_mount`,
    /// `identity_mismatch`, `identity_pins_exhausted`, `invalid_writable_dir`.
    pub kind: &'static str,
    /// Command being wrapped (binary path as passed by the caller).
    pub command: String,
    /// Related path, when the event is about one mount or file.
    pub path: Option<String>,
    /// Short fixed-vocabulary reason, no secrets.
    pub detail: String,
}

impl SandboxEvent {
    #[must_use]
    pub fn fallback_unsandboxed(command: &str, reason: &'static str) -> Self {
        Self {
            kind: "fallback_unsandboxed",
            command: command.to_string(),
            path: None,
            detail: reason.to_string(),
        }
    }

    #[must_use]
    pub fn skipped_mount(command: &str, path: &str, reason: &'static str) -> Self {
        Self {
            kind: "skipped_mount",
            command: command.to_string(),
            path: Some(path.to_string()),
            detail: reason.to_string(),
        }
    }

    #[must_use]
    pub fn identity_mismatch(command: &str, expected: &str, pinned: &str) -> Self {
        Self {
            kind: "identity_mismatch",
            command: command.to_string(),
            path: None,
            detail: format!("binary {expected} previously pinned to {pinned}"),
        }
    }

    #[must_use]
    pub fn identity_pins_exhausted(command: &str) -> Self {
        Self {
            kind: "identity_pins_exhausted",
            command: command.to_string(),
            path: None,
            detail: "binary identity pin table full; match by name only".to_string(),
        }
    }

    #[must_use]
    pub fn invalid_writable_dir(command: &str, candidate: &str) -> Self {
        Self {
            kind: "invalid_writable_dir",
            command: command.to_string(),
            path: Some(candidate.to_string()),
            detail: "writable dir must be $HOME-relative with no .. or absolute path".to_string(),
        }
    }
}

/// Emit one event as a single stderr line. Payload-free by construction:
/// callers pass fixed-vocabulary reasons, never file contents or secrets.
pub fn emit(event: &SandboxEvent) {
    let mut line = String::from("flare-sandbox event=");
    line.push_str(event.kind);
    line.push_str(" command=");
    append_escaped(&mut line, &event.command);
    if let Some(path) = &event.path {
        line.push_str(" path=");
        append_escaped(&mut line, path);
    }
    line.push_str(" detail=");
    append_escaped(&mut line, &event.detail);
    eprintln!("{line}");
}

fn append_escaped(out: &mut String, value: &str) {
    if value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
    {
        out.push_str(value);
    } else {
        let _ = write!(out, "{value:?}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_event_carries_command_and_reason() {
        let e = SandboxEvent::fallback_unsandboxed("claude", "bwrap-not-found");
        assert_eq!(e.kind, "fallback_unsandboxed");
        assert_eq!(e.command, "claude");
        assert_eq!(e.detail, "bwrap-not-found");
        assert!(e.path.is_none());
    }

    #[test]
    fn skipped_mount_event_carries_path() {
        let e = SandboxEvent::skipped_mount("opencode", ".ssh", "symlink-escape");
        assert_eq!(e.path.as_deref(), Some(".ssh"));
    }
}
