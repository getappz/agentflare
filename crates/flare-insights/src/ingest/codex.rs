use chrono::{DateTime, Utc};
use walkdir::WalkDir;

use crate::config::InsightsConfig;
use crate::ingest::{
    common::{extract_tokens, parse_timestamp},
    Adapter, IngestBundle, IngestError,
};
use crate::model::{Session, SessionSource, SessionStatus, TokenUsage, ToolCall, Turn};

pub struct CodexAdapter;

impl Adapter for CodexAdapter {
    fn source_name(&self) -> &'static str {
        "codex"
    }
    fn scan(&self, config: &InsightsConfig) -> Result<IngestBundle, IngestError> {
        let Some(dir) = config.sources.get("codex") else {
            return Ok(IngestBundle::default());
        };
        if !dir.exists() {
            return Ok(IngestBundle::default());
        }
        let mut bundle = IngestBundle::default();
        for entry in WalkDir::new(dir)
            .max_depth(5)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.path().extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some((s, turns, tools)) = parse_codex_jsonl(entry.path()) {
                bundle.sessions.push(s);
                bundle.turns.extend(turns);
                bundle.tool_calls.extend(tools);
            }
        }
        Ok(bundle)
    }
}

fn parse_codex_jsonl(path: &std::path::Path) -> Option<(Session, Vec<Turn>, Vec<ToolCall>)> {
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
    let mut model: Option<String> = None;
    let mut updated_at: Option<DateTime<Utc>> = None;
    let mut cwd: Option<String> = None;
    let mut turns: Vec<Turn> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut seq: u32 = 0;

    for v in &vals {
        if let Some(ts) = v.get("timestamp").and_then(parse_timestamp) {
            updated_at = Some(updated_at.map_or(ts, |prev| prev.max(ts)));
        }
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
        let role = v
            .get("role")
            .and_then(|r| r.as_str())
            .or_else(|| {
                v.get("payload")
                    .and_then(|p| p.get("role"))
                    .and_then(|r| r.as_str())
            })
            .unwrap_or("");
        let is_user = role == "user";
        let is_assistant = role == "assistant";

        let usage = v
            .get("usage")
            .or_else(|| v.get("payload").and_then(|p| p.get("usage")));
        if let Some(u) = usage {
            let t = extract_tokens(u);
            tokens.input += t.input;
            tokens.output += t.output;
            tokens.cache_read += t.cache_read;
            tokens.cache_write += t.cache_write;
            tokens.reasoning += t.reasoning;
        }

        if is_user || is_assistant {
            seq += 1;
            let payload = v.get("payload");
            let text = extract_codex_text(payload, v);
            let (user_text, assistant_text) = if is_user {
                (Some(text), None)
            } else {
                (None, Some(text))
            };
            let started_at = v.get("timestamp").and_then(parse_timestamp);
            let turn_tokens = usage.map(extract_tokens);
            turns.push(Turn {
                id: format!("{}-{}", id, seq),
                session_id: id.clone(),
                seq,
                user_text,
                assistant_text,
                started_at,
                ended_at: started_at,
                tokens: turn_tokens,
                cost_usd: None,
            });
        }

        let payload_type = v
            .get("payload")
            .and_then(|p| p.get("type"))
            .and_then(|t| t.as_str())
            .unwrap_or("");
        if payload_type == "function_call" {
            let call_id = v
                .get("payload")
                .and_then(|p| p.get("call_id"))
                .and_then(|c| c.as_str())
                .unwrap_or(&format!("{}-call-{}", id, tool_calls.len()))
                .to_string();
            let name = v
                .get("payload")
                .and_then(|p| p.get("name"))
                .and_then(|n| n.as_str())
                .unwrap_or("unknown")
                .to_string();
            let input = v
                .get("payload")
                .and_then(|p| p.get("arguments"))
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            tool_calls.push(ToolCall {
                id: call_id,
                session_id: id.clone(),
                turn_seq: seq.max(1),
                name,
                input,
                output: None,
                status: "completed".into(),
                duration_ms: None,
                created_at: v.get("timestamp").and_then(parse_timestamp),
            });
        }
    }

    let project = cwd
        .as_deref()
        .and_then(|c| c.rsplit('/').next())
        .unwrap_or("codex")
        .to_string();

    let session = Session {
        id: id.clone(),
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
        tokens: tokens.clone(),
        cost: None,
        turn_count: turns.len() as u32,
        tool_call_count: tool_calls.len() as u32,
        subagent_count: 0,
        tags: vec![],
        starred: false,
        pid: None,
        cwd: None,
    };

    Some((session, turns, tool_calls))
}

fn extract_codex_text(payload: Option<&serde_json::Value>, v: &serde_json::Value) -> String {
    if let Some(p) = payload {
        if let Some(content) = p.get("content").and_then(|c| c.as_array()) {
            let mut out = String::new();
            for item in content {
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(text);
                }
            }
            if !out.is_empty() {
                return out;
            }
        }
        if let Some(text) = p.get("text").and_then(|t| t.as_str()) {
            return text.to_string();
        }
    }
    v.get("content")
        .and_then(|c| c.as_str())
        .unwrap_or("")
        .to_string()
}
