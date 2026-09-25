//! On-demand single-session loads from foreign-agent stores.
//!
//! Reuses `flare_insights::ingest` adapters strictly read-only: scan one
//! source, filter to the requested session id, hand the parts to
//! `body::build`. No DB upserts, no cursor writes, no foreign-file
//! mutation — a full `insights sync` is the bulk path; this is the
//! single-session path for `handoff send`. Unknown sources and missing
//! sessions degrade to guidance errors (manual handoff), never panics.

use flare_insights::ingest::Adapter;
use flare_insights::model::{FileEvent, Session, ToolCall, Turn};

/// Sources with a reusable ingest adapter, in auto-detect order.
pub const SUPPORTED: &[&str] = &["claude_code", "codex", "opencode"];

/// One foreign session with its parts, ready for [`super::body::build`].
#[derive(Debug)]
pub struct SessionBundle {
    pub session: Session,
    pub turns: Vec<Turn>,
    pub tools: Vec<ToolCall>,
    pub files: Vec<FileEvent>,
    pub subagent_count: usize,
}

/// Canonical source name for a user-supplied alias (`cc`/`claude`,
/// `oc`, `codex`, `auto` passes through for multi-source probing).
/// Kebab-case registry names (`claude-code`, `gemini-cli`) are accepted too:
/// the repo canon is kebab-case (`agent-registry`), this module's internal
/// canon is underscores (matching `flare_insights` adapters).
/// `gemini` names a valid failover *target* (recipient); it has no ingest
/// adapter yet, so it never appears in [`SUPPORTED`] scan order.
pub fn normalize_source(input: &str) -> Result<String, String> {
    match input {
        "claude_code" | "claude" | "cc" | "claude-code" | "claude-code-cli" => {
            Ok("claude_code".into())
        }
        "codex" => Ok("codex".into()),
        "opencode" | "oc" => Ok("opencode".into()),
        "gemini" | "gemini-cli" => Ok("gemini".into()),
        "auto" => Ok("auto".into()),
        other => Err(format!(
            "unsupported source '{other}' — use one of: auto, {}",
            SUPPORTED.join(", ")
        )),
    }
}

fn scan_source(
    source: &str,
    config: &flare_insights::config::InsightsConfig,
) -> Result<flare_insights::ingest::IngestBundle, String> {
    let cursors = Default::default();
    let mut noop = |_: usize, _: usize| {};
    let res = match source {
        "claude_code" => {
            flare_insights::ingest::claude::ClaudeAdapter.scan(config, &cursors, &mut noop)
        }
        "codex" => flare_insights::ingest::codex::CodexAdapter.scan(config, &cursors, &mut noop),
        "opencode" => {
            flare_insights::ingest::opencode::OpenCodeAdapter.scan(config, &cursors, &mut noop)
        }
        other => return Err(format!("unsupported source '{other}'")),
    };
    res.map_err(|e| format!("scan {source} failed (schema drift? degrade to manual handoff): {e}"))
}

/// Load one session by id. `source` is a canonical name or `"auto"` (first
/// hit across [`SUPPORTED`] wins). Read-only; the insights DB is untouched.
pub fn load_session(source: &str, session_id: &str) -> Result<SessionBundle, String> {
    let source = normalize_source(source)?;
    let config = flare_insights::config::InsightsConfig::default();
    let order: Vec<String> = if source == "auto" {
        SUPPORTED.iter().map(|s| s.to_string()).collect()
    } else {
        vec![source.clone()]
    };
    let mut scanned = 0usize;
    // In `auto` mode a failing adapter must not hide sessions living in
    // another store: record its error and keep probing. An explicit source
    // still fails fast so real breakage stays loud.
    let mut errors: Vec<String> = Vec::new();
    for name in &order {
        let bundle = match scan_source(name, &config) {
            Ok(b) => b,
            Err(e) if source == "auto" => {
                errors.push(format!("{name}: {e}"));
                continue;
            }
            Err(e) => return Err(e),
        };
        scanned += bundle.sessions.len();
        if let Some(session) = bundle.sessions.into_iter().find(|s| s.id == session_id) {
            let id = session.id.clone();
            return Ok(SessionBundle {
                session,
                turns: bundle
                    .turns
                    .into_iter()
                    .filter(|t| t.session_id == id)
                    .collect(),
                tools: bundle
                    .tool_calls
                    .into_iter()
                    .filter(|t| t.session_id == id)
                    .collect(),
                files: bundle
                    .file_events
                    .into_iter()
                    .filter(|f| f.session_id == id)
                    .collect(),
                subagent_count: bundle
                    .subagents
                    .iter()
                    .filter(|s| s.session_id == id)
                    .count(),
            });
        }
    }
    Err(format!(
        "session not found: {session_id} (scanned {scanned} sessions in '{source}'; run `agentflare insights sync` then `insights list` to find ids, or paste the transcript manually){}",
        if errors.is_empty() {
            String::new()
        } else {
            format!(" scan errors: {}", errors.join("; "))
        }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// Env-backed config (`CLAUDE_PROJECTS_DIR`) is process-global: serialize
    /// these tests so parallel tests can't observe a half-set environment.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn fixture_dir(tag: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join(tag);
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("ses-1.jsonl"),
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"fix login\"},\"timestamp\":\"2026-09-25T10:00:00.000Z\"}\n\
             {\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"on it\",\"model\":\"claude-opus-4\"},\"timestamp\":\"2026-09-25T10:01:00.000Z\"}\n",
        )
        .unwrap();
        tmp
    }

    #[test]
    fn aliases_normalize_and_unknown_errors() {
        assert_eq!(normalize_source("cc").unwrap(), "claude_code");
        assert_eq!(normalize_source("claude").unwrap(), "claude_code");
        assert_eq!(normalize_source("oc").unwrap(), "opencode");
        assert_eq!(normalize_source("auto").unwrap(), "auto");
        assert_eq!(normalize_source("gemini").unwrap(), "gemini");
        assert_eq!(normalize_source("gemini-cli").unwrap(), "gemini");
        assert_eq!(normalize_source("claude-code").unwrap(), "claude_code");
        assert!(normalize_source("cursor").is_err());
    }

    #[test]
    fn loads_claude_session_from_env_pointed_dir() {
        let _guard = env_lock().lock().unwrap();
        let tmp = fixture_dir("proj");
        // SAFETY: `env_lock` serializes all env-mutating tests in this module;
        // no other thread reads this var concurrently.
        unsafe {
            std::env::set_var("CLAUDE_PROJECTS_DIR", tmp.path());
        }
        let res = load_session("claude", "ses-1");
        // SAFETY: same serialization as above; var is ours alone.
        unsafe {
            std::env::remove_var("CLAUDE_PROJECTS_DIR");
        }
        let bundle = res.unwrap();
        assert_eq!(bundle.session.id, "ses-1");
        assert_eq!(bundle.turns.len(), 2);
        assert_eq!(bundle.session.source.as_str(), "claude_code");
    }

    #[test]
    fn missing_session_names_the_source() {
        let _guard = env_lock().lock().unwrap();
        let tmp = fixture_dir("proj");
        // SAFETY: `env_lock` serializes all env-mutating tests in this module;
        // no other thread reads this var concurrently.
        unsafe {
            std::env::set_var("CLAUDE_PROJECTS_DIR", tmp.path());
        }
        let err = load_session("claude_code", "nope").unwrap_err();
        // SAFETY: same serialization as above; var is ours alone.
        unsafe {
            std::env::remove_var("CLAUDE_PROJECTS_DIR");
        }
        assert!(err.contains("nope"));
    }

    #[test]
    fn send_publishes_versioned_artifact_to_target_inbox() {
        let _guard = env_lock().lock().unwrap();
        let tmp = fixture_dir("proj");
        let out = tempfile::tempdir().unwrap();
        // SAFETY: `env_lock` serializes all env-mutating tests in this module;
        // no other thread reads this var concurrently.
        unsafe {
            std::env::set_var("CLAUDE_PROJECTS_DIR", tmp.path());
        }
        let res = crate::handoff::send(crate::handoff::SendRequest {
            source: "auto".into(),
            session_id: "ses-1".into(),
            target: "opencode".into(),
            verbosity: "minimal".into(),
            thread: Some("t-1".into()),
            reply_to: None,
            name: None,
            artifact_dir: Some(out.path().to_path_buf()),
            depth: 0,
        });
        // SAFETY: same serialization as above; var is ours alone.
        unsafe {
            std::env::remove_var("CLAUDE_PROJECTS_DIR");
        }
        let outcome = res.unwrap();
        assert_eq!(outcome.version, 1);
        assert_eq!(outcome.thread_id, "t-1");
        assert_eq!(outcome.recipient, "opencode");
        let store = agentflare_artifacts::ArtifactStore::new(out.path().to_path_buf());
        let art = store.get(&outcome.id).unwrap();
        assert_eq!(art.sender.as_deref(), Some("claude_code"));
        assert_eq!(art.recipient.as_deref(), Some("opencode"));
        assert!(
            art.content.contains("fix login") || art.content.contains("ses-1"),
            "handoff carries source context"
        );
    }
}
