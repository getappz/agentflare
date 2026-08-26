use chrono::{DateTime, Utc};
use walkdir::WalkDir;

use crate::config::InsightsConfig;
use crate::ingest::{Adapter, IngestError};
use crate::model::{Session, SessionSource, SessionStatus, TokenUsage};

pub struct CodexAdapter;

impl Adapter for CodexAdapter {
    fn source_name(&self) -> &'static str {
        "codex"
    }
    fn scan(&self, config: &InsightsConfig) -> Result<Vec<Session>, IngestError> {
        let Some(dir) = config.sources.get("codex") else {
            return Ok(vec![]);
        };
        if !dir.exists() {
            return Ok(vec![]);
        }
        let mut out = Vec::new();
        for entry in WalkDir::new(dir).max_depth(5).into_iter().filter_map(|e| e.ok()) {
            if entry.path().extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(s) = parse_codex_jsonl(entry.path()) {
                out.push(s);
            }
        }
        Ok(out)
    }
}

fn parse_codex_jsonl(path: &std::path::Path) -> Option<Session> {
    let vals = crate::ingest::read_jsonl_sessions(path);
    if vals.is_empty() {
        return None;
    }
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut tokens = TokenUsage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        reasoning: 0,
    };
    let mut turn_count = 0u32;
    let mut tool_calls = 0u32;
    let mut model: Option<String> = None;
    let mut updated_at: Option<DateTime<Utc>> = None;
    let mut cwd: Option<String> = None;

    for v in &vals {
        // timestamp
        if let Some(ts) = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
        {
            updated_at = Some(updated_at.map_or(ts, |prev| prev.max(ts)));
        }
        // cwd from session_meta
        if let Some(p) = v
            .get("payload")
            .and_then(|p| p.get("cwd"))
            .and_then(|c| c.as_str())
        {
            cwd = Some(p.to_string());
        }
        if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
            model = Some(m.to_string());
        }
        // role can be top-level or payload.role
        let role = v
            .get("role")
            .and_then(|r| r.as_str())
            .or_else(|| v.get("payload").and_then(|p| p.get("role")).and_then(|r| r.as_str()))
            .unwrap_or("");
        if role == "user" {
            turn_count += 1;
        }
        // usage
        let usage = v.get("usage").or_else(|| v.get("payload").and_then(|p| p.get("usage")));
        if let Some(u) = usage {
            tokens.input += u.get("input_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
            tokens.output += u.get("output_tokens").and_then(|n| n.as_u64()).unwrap_or(0);
            tokens.cache_read += u
                .get("cache_read_input_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
        }
        // tool_calls: payload.type == "function_call" or content contains tool
        let payload_type = v
            .get("payload")
            .and_then(|p| p.get("type"))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if payload_type == "function_call" {
            tool_calls += 1;
        }
    }

    let project = cwd
        .as_deref()
        .and_then(|c| c.rsplit('/').next())
        .unwrap_or("codex")
        .to_string();

    Some(Session {
        id,
        source: SessionSource::Codex,
        project,
        project_path: cwd,
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
        turn_count: if turn_count == 0 { 1 } else { turn_count },
        tool_call_count: tool_calls,
        subagent_count: 0,
        tags: vec![],
        starred: false,
        pid: None,
        cwd: None,
    })
}
