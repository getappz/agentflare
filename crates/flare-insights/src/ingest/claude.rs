use std::path::Path;

use chrono::{DateTime, Utc};
use walkdir::WalkDir;

use crate::config::InsightsConfig;
use crate::ingest::{Adapter, IngestError};
use crate::model::{Session, SessionSource, SessionStatus, TokenUsage};

pub struct ClaudeAdapter;

impl Adapter for ClaudeAdapter {
    fn source_name(&self) -> &'static str { "claude_code" }

    fn scan(&self, config: &InsightsConfig) -> Result<Vec<Session>, IngestError> {
        let Some(dir) = config.sources.get("claude_code") else { return Ok(vec![]); };
        if !dir.exists() { return Ok(vec![]); }
        let mut out = Vec::new();
        for entry in WalkDir::new(dir).max_depth(4).into_iter().filter_map(|e| e.ok()) {
            if entry.path().extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
            if let Some(s) = parse_claude_jsonl(entry.path()) { out.push(s); }
        }
        Ok(out)
    }
}

fn parse_claude_jsonl(path: &Path) -> Option<Session> {
    let vals = crate::ingest::read_jsonl_sessions(path);
    if vals.is_empty() { return None; }
    // derive id from file stem or first message sessionId
    let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or("unknown").to_string();
    let mut tokens = TokenUsage { input: 0, output: 0, cache_read: 0, cache_write: 0, reasoning: 0 };
    let mut turn_count = 0u32;
    let mut tool_calls = 0u32;
    let mut model: Option<String> = None;
    let mut updated_at: Option<DateTime<Utc>> = None;
    let mut project = path.parent().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("unknown").to_string();

    for v in &vals {
        if let Some(m) = v.get("model").and_then(|m| m.as_str()) { model = Some(m.to_string()); }
        if let Some(p) = v.get("project").and_then(|p| p.as_str()) { project = p.to_string(); }
        if let Some(ts) = v.get("timestamp").and_then(|t| t.as_str()).and_then(|s| s.parse::<DateTime<Utc>>().ok()) {
            updated_at = Some(updated_at.map_or(ts, |prev| prev.max(ts)));
        }
        let role = v.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role == "user" { turn_count += 1; }
        if let Some(usage) = v.get("usage") {
            tokens.input += usage.get("input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
            tokens.output += usage.get("output_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
            tokens.cache_read += usage.get("cache_read_input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
            tokens.cache_write += usage.get("cache_creation_input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
        }
        if let Some(content) = v.get("content").and_then(|c| c.as_array()) {
            for c in content {
                if c.get("type").and_then(|t| t.as_str()) == Some("tool_use") { tool_calls += 1; }
            }
        }
    }

    Some(Session {
        id,
        source: SessionSource::ClaudeCode,
        project,
        project_path: path.parent().map(|p| p.to_string_lossy().to_string()),
        title: None,
        model,
        status: SessionStatus::Completed,
        awaiting_reason: None,
        started_at: updated_at,
        updated_at: updated_at.unwrap_or_else(Utc::now),
        ended_at: updated_at,
        duration_secs: None,
        tokens,
        cost: None,
        turn_count,
        tool_call_count: tool_calls,
        subagent_count: 0,
        tags: vec![],
        starred: false,
        pid: None,
        cwd: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    #[test]
    fn empty_dir_returns_empty() {
        let dir = TempDir::new().unwrap();
        let cfg = InsightsConfig { sources: [("claude_code".to_string(), dir.path().to_path_buf())].into(), ..Default::default() };
        let a = ClaudeAdapter;
        assert!(a.scan(&cfg).unwrap().is_empty());
    }
}
