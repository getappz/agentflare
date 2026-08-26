use std::path::Path;

use chrono::{DateTime, Utc};
use walkdir::WalkDir;

use crate::config::InsightsConfig;
use crate::ingest::{
    common::{extract_tokens, parse_timestamp, title_from_text},
    IngestBundle, IngestError, Adapter,
};
use crate::model::{Session, SessionSource, SessionStatus, TokenUsage, Turn, ToolCall};

pub struct ClaudeAdapter;

impl Adapter for ClaudeAdapter {
    fn source_name(&self) -> &'static str {
        "claude_code"
    }

    fn scan(&self, config: &InsightsConfig) -> Result<IngestBundle, IngestError> {
        let Some(dir) = config.sources.get("claude_code") else {
            return Ok(IngestBundle::default());
        };
        if !dir.exists() {
            return Ok(IngestBundle::default());
        }
        let mut bundle = IngestBundle::default();
        for entry in WalkDir::new(dir).max_depth(5).into_iter().filter_map(|e| e.ok()) {
            if entry.path().extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some((s, turns, tools)) = parse_claude_jsonl(entry.path()) {
                bundle.sessions.push(s);
                bundle.turns.extend(turns);
                bundle.tool_calls.extend(tools);
            }
        }
        Ok(bundle)
    }
}

fn parse_claude_jsonl(path: &Path) -> Option<(Session, Vec<Turn>, Vec<ToolCall>)> {
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
    let mut tool_call_count = 0u32;
    let mut model: Option<String> = None;
    let mut updated_at: Option<DateTime<Utc>> = None;
    let mut project = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut title: Option<String> = None;
    let mut turns: Vec<Turn> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut seq: u32 = 0;

    for v in &vals {
        if let Some(cwd) = v.get("cwd").and_then(|c| c.as_str()) {
            if let Some(last) = cwd.rsplit('/').next() {
                if !last.is_empty() {
                    project = last.to_string();
                }
            }
        }
        if let Some(ts) = v.get("timestamp").and_then(|t| parse_timestamp(t)) {
            updated_at = Some(updated_at.map_or(ts, |prev| prev.max(ts)));
        }
        if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
            model = Some(m.to_string());
        }
        if let Some(m) = v
            .get("message")
            .and_then(|m| m.get("model"))
            .and_then(|m| m.as_str())
        {
            model = Some(m.to_string());
        }

        let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");

        // DRY: extract usage via common
        let usage = v.get("usage").or_else(|| v.get("message").and_then(|m| m.get("usage")));
        if let Some(u) = usage {
            let t = extract_tokens(u);
            tokens.input += t.input;
            tokens.output += t.output;
            tokens.cache_read += t.cache_read;
            tokens.cache_write += t.cache_write;
            tokens.reasoning += t.reasoning;
        }

        // Build turns: each type=="user" or "assistant" becomes a turn
        if typ == "user" || typ == "human" || typ == "assistant" {
            seq += 1;
            let msg = v.get("message");
            let (user_text, assistant_text) = if typ == "user" || typ == "human" {
                let text = extract_text_from_message(msg);
                if title.is_none() && !text.is_empty() {
                    title = Some(title_from_text(&text, 80));
                }
                (Some(text), None)
            } else {
                let text = extract_text_from_message(msg);
                (None, Some(text))
            };
            let started_at = v.get("timestamp").and_then(|t| parse_timestamp(t));
            // tokens per turn if present
            let turn_tokens = usage.map(extract_tokens);
            let turn_id = format!("{}-{}", id, seq);
            turns.push(Turn {
                id: turn_id,
                session_id: id.clone(),
                seq,
                user_text,
                assistant_text,
                started_at,
                ended_at: started_at,
                tokens: turn_tokens,
                cost_usd: None,
            });

            // tool_calls inside message.content[]
            let content_val = msg.and_then(|m| m.get("content"));
            if let Some(arr) = content_val.and_then(|c| c.as_array()) {
                for c in arr {
                    if c.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        tool_call_count += 1;
                        let call_id = c
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&format!("{}-{}-tool-{}", id, seq, tool_calls.len()))
                            .to_string();
                        let name = c
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let input = c.get("input").cloned().unwrap_or(serde_json::Value::Null);
                        tool_calls.push(ToolCall {
                            id: call_id,
                            session_id: id.clone(),
                            turn_seq: seq,
                            name,
                            input,
                            output: None,
                            status: "completed".into(),
                            duration_ms: None,
                            created_at: started_at,
                        });
                    }
                }
            }
        }

        // Also count direct content tool_use (outside message)
        if typ != "assistant" && typ != "user" && typ != "human" {
            if let Some(arr) = v.get("content").and_then(|c| c.as_array()) {
                for c in arr {
                    if c.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        tool_call_count += 1;
                    }
                }
            }
        }
    }

    let session = Session {
        id: id.clone(),
        source: SessionSource::ClaudeCode,
        project,
        project_path: path.parent().map(|p| p.to_string_lossy().to_string()),
        title,
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
        tool_call_count,
        subagent_count: 0,
        tags: vec![],
        starred: false,
        pid: None,
        cwd: None,
    };

    Some((session, turns, tool_calls))
}

fn extract_text_from_message(msg: Option<&serde_json::Value>) -> String {
    let Some(msg) = msg else { return String::new(); };
    if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
        return s.to_string();
    }
    if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
        let mut out = String::new();
        for item in arr {
            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        return out;
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    #[test]
    fn empty_dir_returns_empty() {
        let dir = TempDir::new().unwrap();
        let cfg = InsightsConfig {
            sources: [("claude_code".to_string(), dir.path().to_path_buf())].into(),
            ..Default::default()
        };
        let a = ClaudeAdapter;
        assert!(a.scan(&cfg).unwrap().sessions.is_empty());
    }

    #[test]
    fn parses_user_and_tool_calls() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.jsonl");
        let content = r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}
{"type":"assistant","timestamp":"2026-01-01T00:01:00Z","message":{"model":"claude-sonnet-4","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":2},"content":[{"type":"tool_use","id":"call1","name":"Bash","input":{}}]}}
"#;
        std::fs::write(&file, content).unwrap();
        let cfg = InsightsConfig {
            sources: [("claude_code".to_string(), dir.path().to_path_buf())].into(),
            ..Default::default()
        };
        let bundle = ClaudeAdapter.scan(&cfg).unwrap();
        assert_eq!(bundle.sessions.len(), 1);
        assert_eq!(bundle.sessions[0].turn_count, 2);
        assert_eq!(bundle.tool_calls.len(), 1);
        assert_eq!(bundle.sessions[0].tokens.input, 10);
    }
}
