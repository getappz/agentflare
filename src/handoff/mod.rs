//! Agentflare-native cross-agent handoff I/O.
//!
//! Reads foreign-agent sessions read-only from the insights DB (populated
//! by `agentflare insights sync`, which parses `~/.claude`, `opencode.db`,
//! etc. without ever writing to them) and renders the v1 handoff body.
//! Every entry point here is side-effect free except `export_body` writing
//! the one file the caller asked for: `preview`/`verify`/`doctor` print,
//! they never publish.

pub mod apply;
pub mod body;
pub mod route;
pub mod sources;

use body::{HandoffBodyV1, build, git_context, render_markdown};
use flare_insights::handoff::Verbosity;
use std::path::{Path, PathBuf};

/// Default insights DB, mirroring `src/cli/insights.rs`.
pub fn default_insights_db() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/share/agentflare/insights/observatory.db")
}

fn parse_verbosity(s: &str) -> Verbosity {
    match s {
        "minimal" => Verbosity::Minimal,
        "verbose" => Verbosity::Verbose,
        "full" => Verbosity::Full,
        _ => Verbosity::Standard,
    }
}

fn open_store(db: &Path) -> Result<flare_insights::store::InsightsStore, String> {
    if !db.exists() {
        return Err(format!(
            "insights DB not found at {} — run `agentflare insights sync` first",
            db.display()
        ));
    }
    flare_insights::store::InsightsStore::open(db)
        .map_err(|e| format!("cannot open insights DB {}: {e}", db.display()))
}

/// Fetch session parts and build the v1 body. Read-only: opens the insights
/// DB (which must already exist) and runs `git` probes at most.
pub fn load_body(
    db: &Path,
    session_id: &str,
    target: &str,
    verbosity: &str,
) -> Result<HandoffBodyV1, String> {
    let store = open_store(db)?;
    let session = store
        .get_session(session_id)
        .map_err(|e| format!("session lookup failed: {e}"))?
        .ok_or_else(|| format!("session not found: {session_id}"))?;
    let max_turns = parse_verbosity(verbosity).max_turns();
    // A handoff built on silently dropped turns/tools would look complete
    // while missing context: fail loudly instead of shipping it partial.
    let turns = store
        .get_turns(&session.id)
        .map_err(|e| format!("turn lookup failed: {e}"))?;
    let tools = store
        .get_tool_calls(&session.id)
        .map_err(|e| format!("tool lookup failed: {e}"))?;
    let files = store
        .get_file_events(&session.id)
        .map_err(|e| format!("file lookup failed: {e}"))?;
    let subagents = store
        .get_subagents(&session.id)
        .map_err(|e| format!("subagent lookup failed: {e}"))?;
    let git = git_context(session.cwd.as_deref());
    Ok(build(
        &session,
        &turns,
        &tools,
        &files,
        subagents.len(),
        target,
        max_turns,
        git,
        0,
    ))
}

/// Render the handoff markdown to stdout. Zero writes.
pub fn preview(
    db: Option<PathBuf>,
    session_id: &str,
    target: &str,
    verbosity: &str,
) -> Result<String, String> {
    let body = load_body(
        &db.unwrap_or_else(default_insights_db),
        session_id,
        target,
        verbosity,
    )?;
    Ok(render_markdown(&body))
}

/// Render and write to `out` (`md`|`json`), or return stdout text when
/// `out` is `None`. The only writer in this module, and only to the
/// caller-chosen path.
pub fn export_body(
    db: Option<PathBuf>,
    session_id: &str,
    target: &str,
    verbosity: &str,
    format: &str,
    out: Option<PathBuf>,
) -> Result<String, String> {
    let body = load_body(
        &db.unwrap_or_else(default_insights_db),
        session_id,
        target,
        verbosity,
    )?;
    let text = match format {
        "json" => serde_json::to_string_pretty(&body).map_err(|e| format!("encode: {e}"))?,
        _ => render_markdown(&body),
    };
    match out {
        Some(path) => std::fs::write(&path, &text)
            .map(|()| format!("exported handoff to {}", path.display()))
            .map_err(|e| format!("cannot write {}: {e}", path.display())),
        None => Ok(text),
    }
}

/// Pre-flight report: what a handoff of this session would carry vs drop.
/// Read-only; the JSON is the loss accounting reviewers check.
pub fn verify(db: Option<PathBuf>, session_id: &str, target: &str) -> Result<String, String> {
    let db_path = db.unwrap_or_else(default_insights_db);
    let body = load_body(&db_path, session_id, target, "standard")?;
    let rendered = render_markdown(&body);
    let report = serde_json::json!({
        "session": body.source.session_id,
        "tool": body.source.tool,
        "target": body.target,
        "turns_total": body.counts.turns,
        "turns_carried": body.turns.len(),
        "tools_seen": body.counts.tools,
        "files_seen": body.counts.files,
        "subagents_seen": body.counts.subagents,
        "files_touched": body.files_touched,
        "git": body.git,
        "dropped_fields": body.dropped_fields,
        "rendered_bytes": rendered.len(),
    });
    serde_json::to_string_pretty(&report).map_err(|e| format!("encode: {e}"))
}

/// Health of the handoff path: agentflare memory DB, insights DB, and
/// foreign-agent source dirs. Degrades to manual-handoff guidance when a
/// source is missing — never an error.
pub fn doctor(db: Option<PathBuf>) -> Result<String, String> {
    let mut lines: Vec<String> = Vec::new();
    match crate::memory::store::open() {
        Ok(_) => lines.push("memory db: ok".into()),
        Err(e) => lines.push(format!("memory db: FAILED ({e})")),
    }
    let db_path = db.unwrap_or_else(default_insights_db);
    match open_store(&db_path) {
        Ok(_) => lines.push(format!("insights db: ok ({})", db_path.display())),
        Err(e) => lines.push(format!("insights db: {e}")),
    }
    let config = flare_insights::config::InsightsConfig::default();
    let mut sources: Vec<(&String, &PathBuf)> = config.sources.iter().collect();
    sources.sort_by(|a, b| a.0.cmp(b.0));
    for (name, path) in sources {
        lines.push(format!(
            "source {name}: {}",
            if path.exists() {
                "found"
            } else {
                "missing (manual handoff: paste transcript)"
            }
        ));
    }
    Ok(lines.join("\n"))
}

/// Targeted send: load one foreign session read-only, build the v1 body,
/// publish it as a versioned artifact addressed to the receiving agent.
/// The artifact publish is the only write in this module; sources stay
/// read-only.
/// Inputs for a targeted inbox send; `artifact_dir` overrides the store.
pub struct SendRequest {
    pub source: String,
    pub session_id: String,
    pub target: String,
    pub verbosity: String,
    pub thread: Option<String>,
    pub reply_to: Option<String>,
    pub name: Option<String>,
    pub artifact_dir: Option<PathBuf>,
    /// Failover chain depth carried into the artifact (0 for a fresh hop).
    pub depth: u32,
}

#[derive(Debug)]
/// Address of the published handoff artifact for the receiver to fetch.
pub struct SendOutcome {
    pub id: String,
    pub version: u32,
    pub thread_id: String,
    pub recipient: String,
}

pub fn send(req: SendRequest) -> Result<SendOutcome, String> {
    // The chain cap is enforced here as well as in `route`: a direct send
    // must not overshoot it via an explicit --depth.
    if req.depth >= route::MAX_DEPTH {
        return Err(format!(
            "failover chain depth {} reached (cap {}) — stop and ask a human",
            req.depth,
            route::MAX_DEPTH
        ));
    }
    let bundle = sources::load_session(&req.source, &req.session_id)?;
    let max_turns = parse_verbosity(&req.verbosity).max_turns();
    let git = body::git_context(bundle.session.cwd.as_deref());
    let short: String = bundle.session.id.chars().take(8).collect();
    let target = req.target.clone();
    let sender = bundle.session.source.as_str().to_string();
    let session_ref = bundle.session.id.clone();
    let built = body::build(
        &bundle.session,
        &bundle.turns,
        &bundle.tools,
        &bundle.files,
        bundle.subagent_count,
        &target,
        max_turns,
        git,
        req.depth,
    );
    let content = body::render_markdown(&built);
    let thread_id = req.thread.unwrap_or_else(|| {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        format!("t{nanos}")
    });
    let store: agentflare_artifacts::ArtifactStore = match req.artifact_dir {
        Some(d) => agentflare_artifacts::ArtifactStore::new(d),
        // No silent flat-file fallback: the receiver's inbox reads the DB
        // store, so a fallback artifact would report success while remaining
        // invisible to the recipient. Fail loudly instead.
        None => {
            let s = crate::store::open().map_err(|e| {
                format!("cannot open artifact store: {e} (pass --dir for flat-file output)")
            })?;
            agentflare_artifacts::ArtifactStore::with_store(s)
        }
    };
    let resp = store
        .publish(&agentflare_artifacts::PublishRequest {
            name: req.name.unwrap_or_else(|| format!("handoff-{short}")),
            artifact_type: agentflare_artifacts::ArtifactType::Markdown,
            content,
            session_id: "handoffs".into(),
            update_id: None,
            label: None,
            description: Some(format!("continuity handoff {session_ref} → {target}")),
            favicon: Some("🤝".into()),
            base_version: None,
            sender: Some(sender),
            recipient: Some(target.clone()),
            thread_id: Some(thread_id.clone()),
            reply_to: req.reply_to,
            git: crate::mcp_server::AgentflareMcp::git_provenance(),
        })
        .map_err(|e| e.to_string())?;
    Ok(SendOutcome {
        id: resp.id,
        version: resp.version,
        thread_id,
        recipient: target,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_refuses_depth_at_cap() {
        let err = send(SendRequest {
            source: "auto".into(),
            session_id: "ses-test".into(),
            target: "opencode".into(),
            verbosity: "minimal".into(),
            thread: None,
            reply_to: None,
            name: None,
            artifact_dir: None,
            depth: route::MAX_DEPTH,
        })
        .unwrap_err();
        assert!(err.contains("cap"), "{err}");
    }
}
