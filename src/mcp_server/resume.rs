//! Cross-agent session resume: one-shot "pick up the stopped session here".
//!
//! Two modes, per the `/flare:resume` contract:
//! - explicit `session`: the caller names the foreign session id (from
//!   `agentflare insights list`). Reads the foreign store live via
//!   `handoff::sources::load_session` — no `insights sync` needed — and
//!   publishes the v1 handoff body as an artifact addressed to the
//!   receiving agent (`to`, defaulting to this runtime's own identity).
//! - auto (no `session`): picks the latest session from the insights DB
//!   (needs a prior `agentflare insights sync`), scoped to sessions whose cwd
//!   is the current project and excluding the calling session (by
//!   `CLAUDE_CODE_SESSION_ID`; without it, the receiver's whole source). Because an auto-pick can guess wrong,
//!   the first call is confirm-gated: it returns `needs_confirm` with the
//!   picked session and performs zero writes. Re-calling with
//!   `confirm_session` (or `session`) set to that id performs the send.
//! - `dry_run` renders the handoff markdown without publishing anything.

use super::types::ResumeRequest;
use rmcp::model::ErrorData;

/// Env override for the insights DB path (tests point it at a temp file).
pub(crate) const INSIGHTS_DB_ENV: &str = "AGENTFLARE_INSIGHTS_DB";

/// Env override for the artifact store dir (tests point it at a temp dir so
/// a confirmed send never publishes outside the test).
pub(crate) const ARTIFACTS_DIR_ENV: &str = "AGENTFLARE_ARTIFACTS_DIR";

/// Env var Claude Code sets to the calling session's id.
const CURRENT_SESSION_ENV: &str = "CLAUDE_CODE_SESSION_ID";

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
        let target_canon = target.replace('-', "_");
        // The calling session is excluded by id; when the runtime doesn't
        // expose it, fall back to excluding the receiver's whole source.
        let current = std::env::var(CURRENT_SESSION_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty());
        // Scope to this project: the session's cwd is the project dir or
        // below it (worktrees live under it).
        let project = std::env::current_dir().ok();
        // Paginated: filters run per page so an eligible session past the
        // first page isn't hidden by newer ineligible rows.
        let picked = {
            let mut offset = 0usize;
            loop {
                let page = store
                    .list_sessions(50, offset)
                    .map_err(|e| internal(format!("session list failed: {e}")))?;
                let page_len = page.len();
                let found = page
                    .into_iter()
                    .filter(|s| from == "auto" || s.source.as_str() == from)
                    .filter(|s| match &current {
                        Some(id) => &s.id != id,
                        None => s.source.as_str() != target_canon,
                    })
                    .find(|s| {
                        // OpenCode fills project_path/directory but historically
                        // left cwd null — accept either for project scoping.
                        let root = s.cwd.as_ref().or(s.project_path.as_ref());
                        match (&project, root) {
                            (Some(p), Some(c)) => std::path::Path::new(c).starts_with(p),
                            _ => false,
                        }
                    });
                if found.is_some() || page_len < 50 {
                    break found;
                }
                offset += page_len;
            }
        };
        let picked = picked.ok_or_else(|| {
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
        let artifact_dir = std::env::var(ARTIFACTS_DIR_ENV)
            .ok()
            .filter(|p| !p.trim().is_empty())
            .map(std::path::PathBuf::from);
        let out = crate::handoff::send(crate::handoff::SendRequest {
            source: source.to_string(),
            session_id: session_id.to_string(),
            target: target.to_string(),
            verbosity: verbosity.to_string(),
            thread: None,
            reply_to: None,
            name: None,
            artifact_dir,
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
    use std::sync::Mutex;

    /// Share the env lock with foreign-store tests that also override source paths.
    fn env_lock() -> &'static Mutex<()> {
        &agent_registry::detect::PATH_LOCK
    }

    /// RAII env override: restores the previous value on drop, including panics.
    struct EnvGuard {
        key: &'static str,
        prev: Option<String>,
    }

    impl EnvGuard {
        /// SAFETY: caller must hold `env_lock`; no other thread may read the var.
        fn set(key: &'static str, val: &std::path::Path) -> Self {
            let prev = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, val);
            }
            Self { key, prev }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    /// In-project session: its cwd is this process's cwd.
    fn seed_session(
        store: &flare_insights::store::InsightsStore,
        id: &str,
        source: flare_insights::model::SessionSource,
        updated_at: chrono::DateTime<chrono::Utc>,
    ) {
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        seed_session_in(store, id, source, updated_at, cwd);
    }

    fn seed_session_in(
        store: &flare_insights::store::InsightsStore,
        id: &str,
        source: flare_insights::model::SessionSource,
        updated_at: chrono::DateTime<chrono::Utc>,
        cwd: String,
    ) {
        let session = flare_insights::model::Session {
            id: id.into(),
            source,
            project: "demo".into(),
            project_path: None,
            title: Some("fix login".into()),
            model: None,
            status: flare_insights::model::SessionStatus::Abandoned,
            awaiting_reason: None,
            started_at: None,
            updated_at,
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
            cost_basis: flare_insights::model::CostBasis::Unknown,
            pricing_key: None,
            turn_count: 2,
            tool_call_count: 0,
            subagent_count: 0,
            tags: vec![],
            starred: false,
            pid: None,
            cwd: Some(cwd),
        };
        store.upsert_session(&session).unwrap();
    }

    fn seed_db(path: &std::path::Path) {
        let store = flare_insights::store::InsightsStore::open(path).unwrap();
        seed_session(
            &store,
            "ses-resume-1",
            flare_insights::model::SessionSource::ClaudeCode,
            chrono::Utc::now(),
        );
    }

    /// Foreign-store fixture mirroring `handoff::sources` tests: a live
    /// Claude session the send path can scan without touching real stores.
    fn seed_foreign_session(root: &std::path::Path) {
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(
            proj.join("ses-1.jsonl"),
            "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"fix login\"},\"timestamp\":\"2026-09-25T10:00:00.000Z\"}\n\
             {\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":\"on it\",\"model\":\"claude-opus-4\"},\"timestamp\":\"2026-09-25T10:01:00.000Z\"}\n",
        )
        .unwrap();
    }

    #[test]
    fn explicit_dry_run_reads_live_claude_session() {
        let _lock = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let foreign = tmp.path().join(".claude/projects");
        seed_foreign_session(&foreign);
        let _claude = EnvGuard::set("CLAUDE_PROJECTS_DIR", &foreign);
        let _db = EnvGuard::set(INSIGHTS_DB_ENV, &tmp.path().join("missing.db"));
        let artifacts = tmp.path().join("artifacts");
        let _art = EnvGuard::set(ARTIFACTS_DIR_ENV, &artifacts);
        let server = crate::mcp_server::AgentflareMcp::default();

        for from in [Some("claude_code".into()), None] {
            let out = server
                .resume_impl(crate::mcp_server::types::ResumeRequest {
                    session: Some("ses-1".into()),
                    from,
                    dry_run: Some(true),
                    ..Default::default()
                })
                .unwrap();
            let out: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(out["status"], "preview");
            assert_eq!(out["picked"]["session"], "ses-1");
            assert_eq!(out["picked"]["source"], "claude_code");
            assert!(out["preview"].as_str().unwrap().contains("fix login"));
        }
        assert!(!artifacts.exists(), "dry run must not publish artifacts");
        assert!(!tmp.path().join("missing.db").exists());
    }

    #[test]
    fn unknown_from_is_rejected() {
        let server = crate::mcp_server::AgentflareMcp::default();
        let err = server
            .resume_impl(crate::mcp_server::types::ResumeRequest {
                from: Some("windsurf".into()),
                ..Default::default()
            })
            .unwrap_err();
        assert!(format!("{err:?}").contains("unsupported source"), "{err:?}");
    }

    /// Keep every foreign store and both publish targets inside the fixture.
    /// Caller holds `env_lock` until these guards have been dropped.
    fn antigravity_env(root: &std::path::Path) -> Vec<EnvGuard> {
        let brain = root.join("brain");
        let logs = brain.join("agy-resume-1/.system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let cwd = std::env::current_dir().unwrap();
        let rows = [
            serde_json::json!({
                "type": "USER_INPUT", "source": "USER_EXPLICIT",
                "created_at": "2026-10-27T10:00:00Z",
                "content": "<USER_REQUEST>fix duplicate PR detection</USER_REQUEST>",
            }),
            serde_json::json!({
                "type": "PLANNER_RESPONSE", "source": "MODEL",
                "created_at": "2026-10-27T10:01:00Z",
                "content": "Reuse the existing duplicate matcher",
                "tool_calls": [{"name": "read_file", "args": {"Path": "src/main.rs"}}],
            }),
        ];
        std::fs::write(
            logs.join("transcript.jsonl"),
            rows.iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .unwrap();
        let conn = rusqlite::Connection::open(root.join("conversation_summaries.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversation_summaries (conversation_id TEXT, workspace_uris TEXT);",
        )
        .unwrap();
        let uris =
            serde_json::to_string(&[reqwest::Url::from_directory_path(&cwd).unwrap().to_string()])
                .unwrap();
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1, ?2)",
            rusqlite::params!["agy-resume-1", uris],
        )
        .unwrap();
        let mut guards: Vec<_> = [
            "CLAUDE_PROJECTS_DIR",
            "CODEX_SESSIONS_DIR",
            "OPENCODE_DIR",
            "CURSOR_STATE_VSCDB",
        ]
        .into_iter()
        .map(|key| EnvGuard::set(key, &root.join("missing")))
        .collect();
        guards.push(EnvGuard::set("ANTIGRAVITY_BRAIN_DIR", &brain));
        guards.push(EnvGuard::set(INSIGHTS_DB_ENV, &root.join("observatory.db")));
        guards.push(EnvGuard::set(ARTIFACTS_DIR_ENV, &root.join("artifacts")));
        guards
    }

    #[test]
    fn explicit_antigravity_resume_reads_live_transcript_without_sync() {
        let _lock = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let _env = antigravity_env(tmp.path());
        let server = crate::mcp_server::AgentflareMcp::default();
        for from in ["antigravity", "antigravity-cli", "auto"] {
            let out = server
                .resume_impl(ResumeRequest {
                    session: Some("agy-resume-1".into()),
                    from: Some(from.into()),
                    to: Some("codex".into()),
                    dry_run: Some(true),
                    ..Default::default()
                })
                .unwrap();
            let out: serde_json::Value = serde_json::from_str(&out).unwrap();
            assert_eq!(out["status"], "preview");
            assert_eq!(out["picked"]["source"], "antigravity");
            assert_eq!(out["picked"]["session"], "agy-resume-1");
            let preview = out["preview"].as_str().unwrap();
            assert!(preview.contains("fix duplicate PR detection"));
            assert!(preview.contains("Reuse the existing duplicate matcher"));
        }
        assert!(!tmp.path().join("observatory.db").exists());
        assert!(!tmp.path().join("artifacts").exists());
    }

    #[test]
    fn auto_antigravity_resume_confirms_then_publishes_live_context() {
        use flare_insights::ingest::Adapter;
        let _lock = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let _env = antigravity_env(tmp.path());
        let bundle = flare_insights::ingest::antigravity::AntigravityAdapter
            .scan(&Default::default(), &Default::default(), &mut |_, _| {})
            .unwrap();
        let session = &bundle.sessions[0];
        assert!(session.cwd.is_none());
        assert!(session.project_path.is_some());
        let store =
            flare_insights::store::InsightsStore::open(&tmp.path().join("observatory.db")).unwrap();
        store.upsert_session(session).unwrap();
        let server = crate::mcp_server::AgentflareMcp::default();
        let first = server
            .resume_impl(ResumeRequest {
                from: Some("antigravity".into()),
                to: Some("codex".into()),
                ..Default::default()
            })
            .unwrap();
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first["status"], "needs_confirm");
        assert_eq!(first["picked"]["session"], "agy-resume-1");
        assert!(!tmp.path().join("artifacts").exists());
        let sent = server
            .resume_impl(ResumeRequest {
                from: Some("antigravity".into()),
                to: Some("codex".into()),
                confirm_session: Some("agy-resume-1".into()),
                ..Default::default()
            })
            .unwrap();
        let sent: serde_json::Value = serde_json::from_str(&sent).unwrap();
        assert_eq!(sent["status"], "resumed");
        let artifacts = agentflare_artifacts::ArtifactStore::new(tmp.path().join("artifacts"));
        let artifact = artifacts
            .get(sent["artifact"]["id"].as_str().unwrap())
            .unwrap();
        assert_eq!(artifact.sender.as_deref(), Some("antigravity"));
        assert!(
            artifact
                .content
                .contains("Reuse the existing duplicate matcher")
        );
    }

    #[test]
    fn auto_without_db_points_at_insights_sync() {
        let _lock = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("nope").join("observatory.db");
        let _db = EnvGuard::set(INSIGHTS_DB_ENV, &missing);
        let server = crate::mcp_server::AgentflareMcp::default();
        let err = server
            .resume_impl(crate::mcp_server::types::ResumeRequest::default())
            .unwrap_err();
        assert!(format!("{err:?}").contains("insights sync"), "{err:?}");
    }

    #[test]
    fn auto_pick_needs_confirm_before_sending() {
        let _lock = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("observatory.db");
        seed_db(&db);
        let _db = EnvGuard::set(INSIGHTS_DB_ENV, &db);
        // Hermetic publish target: a confirmed send must never reach the
        // shared artifact store.
        let artifacts = tmp.path().join("artifacts");
        let _art = EnvGuard::set(ARTIFACTS_DIR_ENV, &artifacts);
        let _claude = EnvGuard::set("CLAUDE_PROJECTS_DIR", &tmp.path().join(".claude/projects"));
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
        // store, which this test doesn't populate — the gate passed).
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
    }

    #[test]
    fn confirmed_send_publishes_hermetically() {
        let _lock = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let foreign = tmp.path().join("claude");
        seed_foreign_session(&foreign);
        let db = tmp.path().join("observatory.db");
        let store = flare_insights::store::InsightsStore::open(&db).unwrap();
        seed_session(
            &store,
            "ses-1",
            flare_insights::model::SessionSource::ClaudeCode,
            chrono::Utc::now(),
        );
        let artifacts = tmp.path().join("artifacts");
        let _db = EnvGuard::set(INSIGHTS_DB_ENV, &db);
        let _art = EnvGuard::set(ARTIFACTS_DIR_ENV, &artifacts);
        // SAFETY: `env_lock` serializes all env-mutating tests in this module.
        let _claude = EnvGuard::set("CLAUDE_PROJECTS_DIR", &foreign);
        let server = crate::mcp_server::AgentflareMcp::default();

        let out = server
            .resume_impl(crate::mcp_server::types::ResumeRequest {
                from: Some("claude_code".into()),
                to: Some("opencode".into()),
                confirm_session: Some("ses-1".into()),
                ..Default::default()
            })
            .unwrap();
        assert!(out.contains("\"status\": \"resumed\""), "{out}");
        let id = serde_json::from_str::<serde_json::Value>(&out).unwrap()["artifact"]["id"]
            .as_str()
            .unwrap()
            .to_string();
        let art = agentflare_artifacts::ArtifactStore::new(artifacts)
            .get(&id)
            .unwrap();
        assert_eq!(art.recipient.as_deref(), Some("opencode"));
        assert!(art.content.contains("fix login"), "handoff carries context");
    }

    #[test]
    fn auto_pick_searches_past_the_first_page() {
        let _lock = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("observatory.db");
        let store = flare_insights::store::InsightsStore::open(&db).unwrap();
        let now = chrono::Utc::now();
        // 50 newer ineligible rows (other project) push the one eligible
        // session past the first page.
        for i in 0..50 {
            seed_session_in(
                &store,
                &format!("ses-elsewhere-{i}"),
                flare_insights::model::SessionSource::OpenCode,
                now - chrono::Duration::seconds(i as i64),
                "/nonexistent/other-project".into(),
            );
        }
        seed_session(
            &store,
            "ses-old-claude",
            flare_insights::model::SessionSource::ClaudeCode,
            now - chrono::Duration::seconds(3600),
        );
        let _db = EnvGuard::set(INSIGHTS_DB_ENV, &db);
        let server = crate::mcp_server::AgentflareMcp::default();

        let out = server
            .resume_impl(crate::mcp_server::types::ResumeRequest {
                from: Some("claude_code".into()),
                to: Some("opencode".into()),
                dry_run: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert!(out.contains("ses-old-claude"), "{out}");
    }

    #[test]
    fn auto_pick_uses_project_path_when_cwd_missing() {
        let _lock = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("observatory.db");
        let store = flare_insights::store::InsightsStore::open(&db).unwrap();
        let cwd = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let now = chrono::Utc::now();
        let session = flare_insights::model::Session {
            id: "ses-oc-path".into(),
            source: flare_insights::model::SessionSource::OpenCode,
            project: "demo".into(),
            project_path: Some(cwd),
            title: Some("opencode path".into()),
            model: None,
            status: flare_insights::model::SessionStatus::Abandoned,
            awaiting_reason: None,
            started_at: None,
            updated_at: now,
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
            cost_basis: flare_insights::model::CostBasis::Unknown,
            pricing_key: None,
            turn_count: 1,
            tool_call_count: 0,
            subagent_count: 0,
            tags: vec![],
            starred: false,
            pid: None,
            cwd: None,
        };
        store.upsert_session(&session).unwrap();
        assert_eq!(session.source.as_str(), "opencode");
        let _db = EnvGuard::set(INSIGHTS_DB_ENV, &db);
        let server = crate::mcp_server::AgentflareMcp::default();
        let out = server
            .resume_impl(crate::mcp_server::types::ResumeRequest {
                from: Some("opencode".into()),
                to: Some("claude_code".into()),
                dry_run: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert!(out.contains("ses-oc-path"), "{out}");
    }

    #[test]
    fn auto_pick_skips_calling_session_and_other_projects() {
        let _lock = env_lock().lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("observatory.db");
        let store = flare_insights::store::InsightsStore::open(&db).unwrap();
        let now = chrono::Utc::now();
        let cc = flare_insights::model::SessionSource::ClaudeCode;
        seed_session(&store, "ses-self", cc.clone(), now);
        seed_session_in(
            &store,
            "ses-elsewhere",
            cc.clone(),
            now - chrono::Duration::seconds(10),
            "/nonexistent/other-project".into(),
        );
        seed_session(&store, "ses-prev", cc, now - chrono::Duration::seconds(60));
        let _db = EnvGuard::set(INSIGHTS_DB_ENV, &db);
        let _me = EnvGuard::set(CURRENT_SESSION_ENV, std::path::Path::new("ses-self"));
        let server = crate::mcp_server::AgentflareMcp::default();

        let out = server
            .resume_impl(crate::mcp_server::types::ResumeRequest {
                from: Some("claude_code".into()),
                to: Some("claude-code".into()),
                dry_run: Some(true),
                ..Default::default()
            })
            .unwrap();
        assert!(out.contains("ses-prev"), "{out}");
    }
}
