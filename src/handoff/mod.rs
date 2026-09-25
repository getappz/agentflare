//! Agentflare-native cross-agent handoff I/O.
//!
//! Reads foreign-agent sessions read-only from the insights DB (populated
//! by `agentflare insights sync`, which parses `~/.claude`, `opencode.db`,
//! etc. without ever writing to them) and renders the v1 handoff body.
//! Every entry point here is side-effect free except `export_body` writing
//! the one file the caller asked for: `preview`/`verify`/`doctor` print,
//! they never publish.

pub mod body;

use body::{HandoffBodyV1, build, git_context, render_markdown};
use flare_insights::handoff::Verbosity;
use std::path::{Path, PathBuf};

/// Default insights DB, mirroring `src/cli/insights.rs`.
pub fn default_insights_db() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
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
    let turns = store.get_turns(&session.id).unwrap_or_default();
    let tools = store.get_tool_calls(&session.id).unwrap_or_default();
    let files = store.get_file_events(&session.id).unwrap_or_default();
    let subagents = store.get_subagents(&session.id).unwrap_or_default();
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
