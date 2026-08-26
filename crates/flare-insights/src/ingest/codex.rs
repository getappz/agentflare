use chrono::{DateTime, Utc};
use walkdir::WalkDir;

use crate::config::InsightsConfig;
use crate::ingest::{Adapter, IngestError};
use crate::model::{Session, SessionSource, SessionStatus, TokenUsage};

pub struct CodexAdapter;

impl Adapter for CodexAdapter {
    fn source_name(&self) -> &'static str { "codex" }
    fn scan(&self, config: &InsightsConfig) -> Result<Vec<Session>, IngestError> {
        let Some(dir) = config.sources.get("codex") else { return Ok(vec![]); };
        if !dir.exists() { return Ok(vec![]); }
        let mut out = Vec::new();
        for entry in WalkDir::new(dir).max_depth(3).into_iter().filter_map(|e| e.ok()) {
            if entry.path().extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
            if let Some(s) = parse_codex_jsonl(entry.path()) { out.push(s); }
        }
        Ok(out)
    }
}

fn parse_codex_jsonl(path: &std::path::Path) -> Option<Session> {
    let vals = crate::ingest::read_jsonl_sessions(path);
    if vals.is_empty() { return None; }
    let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
    let mut tokens = TokenUsage { input: 0, output: 0, cache_read: 0, cache_write: 0, reasoning: 0 };
    let mut turn_count = 0u32;
    let mut updated_at: Option<DateTime<Utc>> = None;
    for v in &vals {
        if let Some(ts) = v.get("timestamp").and_then(|t| t.as_str()).and_then(|s| s.parse::<DateTime<Utc>>().ok()) {
            updated_at = Some(updated_at.map_or(ts, |prev| prev.max(ts)));
        }
        if v.get("role").and_then(|r| r.as_str()) == Some("user") { turn_count += 1; }
        if let Some(u) = v.get("usage") {
            tokens.input += u.get("input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
            tokens.output += u.get("output_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
        }
    }
    Some(Session {
        id, source: SessionSource::Codex, project: "codex".into(), project_path: None,
        title: None, model: None, status: SessionStatus::Completed, awaiting_reason: None,
        started_at: updated_at, updated_at: updated_at.unwrap_or_else(Utc::now), ended_at: updated_at,
        duration_secs: None, tokens, cost: None, turn_count, tool_call_count: 0, subagent_count: 0,
        tags: vec![], starred: false, pid: None, cwd: None,
    })
}
