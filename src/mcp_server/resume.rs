//! Cross-agent session resume: one-shot "pick up the stopped session here".
//!
//! Two modes, per the `/flare:resume` contract:
//! - explicit `session`: the caller names the foreign session id (from
//!   `agentflare insights list`). Reads the foreign store live via
//!   `handoff::sources::load_session` — no `insights sync` needed — and
//!   publishes the v1 handoff body as an artifact addressed to the
//!   receiving agent (`to`, defaulting to this runtime's own identity).
//! - auto (no `session`): picks the latest session from the insights DB
//!   (needs a prior `agentflare insights sync`), excluding sessions that
//!   already belong to the receiver. Because an auto-pick can guess wrong,
//!   the first call is confirm-gated: it returns `needs_confirm` with the
//!   picked session and performs zero writes. Re-calling with
//!   `confirm_session` (or `session`) set to that id performs the send.
//! - `dry_run` renders the handoff markdown without publishing anything.

use super::types::ResumeRequest;
use rmcp::model::ErrorData;

/// Env override for the insights DB path (tests point it at a temp file).
pub(crate) const INSIGHTS_DB_ENV: &str = "AGENTFLARE_INSIGHTS_DB";

pub(crate) fn resume_insights_db() -> std::path::PathBuf {
    match std::env::var(INSIGHTS_DB_ENV) {
        Ok(p) if !p.trim().is_empty() => std::path::PathBuf::from(p),
        _ => crate::handoff::default_insights_db(),
    }
}

fn invalid(msg: impl Into<String>) -> ErrorData {
    ErrorData::invalid_params(msg.into(), None)
}

fn internal(msg: impl Into<String>) -> ErrorData {
    ErrorData::internal_error(msg.into(), None)
}

fn session_json(s: &flare_insights::model::Session) -> serde_json::Value {
    serde_json::json!({
        "session": s.id,
        "source": s.source.as_str(),
        "title": s.title,
        "project": s.project,
        "updated_at": s.updated_at.to_rfc3339(),
        "turns": s.turn_count,
    })
}

impl super::AgentflareMcp {
    pub fn resume_impl(&self, req: ResumeRequest) -> Result<String, ErrorData> {
        let from_raw = req.from.as_deref().unwrap_or("auto");
        let from = crate::handoff::sources::normalize_source(from_raw)
            .map_err(|e| invalid(format!("{e} (resume --from)")))?;
        let verbosity = req.verbosity.as_deref().unwrap_or("standard").to_string();
        let target = match req.to {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => self.agent.clone().unwrap_or_else(|| "opencode".to_string()),
        };
        let dry_run = req.dry_run.unwrap_or(false);

        let explicit = req.session.filter(|s| !s.trim().is_empty());

        if let Some(session_id) = explicit {
            return self.resume_send(&from, &session_id, &target, &verbosity, dry_run);
        }

        // Auto mode: latest session from the insights DB, never our own.
        let db_path = resume_insights_db();
        let store = flare_insights::store::InsightsStore::open(&db_path).map_err(|_| {
            invalid(format!(
                "insights DB not found at {} — run `agentflare insights sync` first, or pass an explicit session id",
                db_path.display()
            ))
        })?;
        let sessions = store
            .list_sessions(50, 0)
            .map_err(|e| internal(format!("session list failed: {e}")))?;
        let target_canon = target.replace('-', "_");
        let picked = sessions
            .into_iter()
            .filter(|s| from == "auto" || s.source.as_str() == from)
            .find(|s| s.source.as_str() != target_canon)
            .ok_or_else(|| {
                invalid(format!(
                    "no resumable session from '{from_raw}' — run `agentflare insights sync` then `insights list` to find ids, or pass session explicitly"
                ))
            })?;

        if dry_run {
            let md = crate::handoff::preview(Some(db_path), &picked.id, &target, &verbosity)
                .map_err(internal)?;
            return Ok(serde_json::to_string_pretty(&serde_json::json!({
                "status": "preview",
                "picked": session_json(&picked),
                "target": target,
                "preview": md,
            }))
            .unwrap_or_default());
        }

        // Confirm gate: an auto-pick must be echoed back before it sends.
        let confirmed = req
            .confirm_session
            .as_deref()
            .is_some_and(|c| c == picked.id);
        if !confirmed {
            return Ok(serde_json::to_string_pretty(&serde_json::json!({
                "status": "needs_confirm",
                "picked": session_json(&picked),
                "target": target,
                "hint": format!(
                    "re-call resume with session=\"{}\" (or confirm_session) to publish the handoff",
                    picked.id
                ),
            }))
            .unwrap_or_default());
        }

        let source = picked.source.as_str().to_string();
        self.resume_send(&source, &picked.id, &target, &verbosity, false)
    }

    /// Explicit send (or confirmed auto-pick): build the v1 body from the
    /// live foreign store and publish it to the receiver's inbox. `dry_run`
    /// renders without publishing.
    fn resume_send(
        &self,
        source: &str,
        session_id: &str,
        target: &str,
        verbosity: &str,
        dry_run: bool,
    ) -> Result<String, ErrorData> {
        if dry_run {
            let bundle =
                crate::handoff::sources::load_session(source, session_id).map_err(invalid)?;
            let max_turns = match verbosity {
                "minimal" => 3,
                "verbose" => 20,
                "full" => 50,
                _ => 10,
            };
            let git = crate::handoff::body::git_context(bundle.session.cwd.as_deref());
            let built = crate::handoff::body::build(
                &bundle.session,
                &bundle.turns,
                &bundle.tools,
                &bundle.files,
                bundle.subagent_count,
                target,
                max_turns,
                git,
                0,
            );
            let md = crate::handoff::body::render_markdown(&built);
            return Ok(serde_json::to_string_pretty(&serde_json::json!({
                "status": "preview",
                "picked": session_json(&bundle.session),
                "target": target,
                "preview": md,
            }))
            .unwrap_or_default());
        }
        let out = crate::handoff::send(crate::handoff::SendRequest {
            source: source.to_string(),
            session_id: session_id.to_string(),
            target: target.to_string(),
            verbosity: verbosity.to_string(),
            thread: None,
            reply_to: None,
            name: None,
            artifact_dir: None,
            depth: 0,
        })
        .map_err(internal)?;
        Ok(serde_json::to_string_pretty(&serde_json::json!({
            "status": "resumed",
            "session": session_id,
            "source": source,
            "target": out.recipient,
            "artifact": {"id": out.id, "version": out.version, "thread": out.thread_id},
            "next": [
                format!("read it: artifact action=get id={} (or /handoff inbox)", out.id),
                "materialize it: `agentflare handoff apply --session <id> --target <agent>` writes the continuity section into AGENTS.md/CLAUDE.md",
            ],
        }))
        .unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// `AGENTFLARE_INSIGHTS_DB` is process-global: serialize env-mutating tests.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn seed_db(path: &std::path::Path) {
        let store = flare_insights::store::InsightsStore::open(path).unwrap();
        let session = flare_insights::model::Session {
            id: "ses-resume-1".into(),
            source: flare_insights::model::SessionSource::ClaudeCode,
            project: "demo".into(),
            project_path: None,
            title: Some("fix login".into()),
            model: None,
            status: flare_insights::model::SessionStatus::Abandoned,
            awaiting_reason: None,
            started_at: None,
            updated_at: chrono::Utc::now(),
            ended_at: None,
            duration_secs: None,
            tokens: flare_insights::model::TokenUsage {
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost: None,
            turn_count: 2,
            tool_call_count: 0,
            subagent_count: 0,
            tags: vec![],
            starred: false,
            pid: None,
            cwd: None,
        };
        store.upsert_session(&session).unwrap();
    }

    #[test]
    fn unknown_from_is_rejected() {
        let server = crate::mcp_server::AgentflareMcp::default();
        let err = server
            .resume_impl(crate::mcp_server::types::ResumeRequest {
                from: Some("cursor".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert!(format!("{err:?}").contains("unsupported source"), "{err:?}");
    }

    #[test]
    fn auto_without_db_points_at_insights_sync() {
        let _guard = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope").join("observatory.db");
        // SAFETY: serialized by env_lock; no other thread reads this var.
        unsafe {
            std::env::set_var(INSIGHTS_DB_ENV, &missing);
        }
        let server = crate::mcp_server::AgentflareMcp::default();
        let err = server
            .resume_impl(crate::mcp_server::types::ResumeRequest::default())
            .unwrap_err();
        unsafe {
            std::env::remove_var(INSIGHTS_DB_ENV);
        }
        assert!(format!("{err:?}").contains("insights sync"), "{err:?}");
    }

    #[test]
    fn auto_pick_needs_confirm_before_sending() {
        let _guard = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("observatory.db");
        seed_db(&db);
        // SAFETY: serialized by env_lock.
        unsafe {
            std::env::set_var(INSIGHTS_DB_ENV, &db);
        }
        let server = crate::mcp_server::AgentflareMcp::default();

        // First call: zero writes, names the picked session.
        let first = server
            .resume_impl(crate::mcp_server::types::ResumeRequest {
                from: Some("claude_code".into()),
                to: Some("opencode".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(first.contains("needs_confirm"), "{first}");
        assert!(first.contains("ses-resume-1"), "{first}");

        // Confirming proceeds past the gate (then fails on the live foreign
        // store, which the fixture doesn't populate — the gate passed).
        let second = server.resume_impl(crate::mcp_server::types::ResumeRequest {
            from: Some("claude_code".into()),
            to: Some("opencode".into()),
            confirm_session: Some("ses-resume-1".into()),
            ..Default::default()
        });
        match second {
            Ok(text) => assert!(!text.contains("needs_confirm"), "{text}"),
            Err(e) => assert!(
                format!("{e:?}").contains("ses-resume-1")
                    || format!("{e:?}").contains("session not found"),
                "{e:?}"
            ),
        }
        unsafe {
            std::env::remove_var(INSIGHTS_DB_ENV);
        }
    }
}
