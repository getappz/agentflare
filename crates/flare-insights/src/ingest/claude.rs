use std::path::Path;

use chrono::{DateTime, Utc};
use walkdir::WalkDir;

use crate::config::InsightsConfig;
use crate::ingest::{Adapter, IngestError};
use crate::model::{Session, SessionSource, SessionStatus, TokenUsage};

pub struct ClaudeAdapter;

impl Adapter for ClaudeAdapter {
    fn source_name(&self) -> &'static str {
        "claude_code"
    }

    fn scan(&self, config: &InsightsConfig) -> Result<Vec<Session>, IngestError> {
        let Some(dir) = config.sources.get("claude_code") else {
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
            if let Some(s) = parse_claude_jsonl(entry.path()) {
                out.push(s);
            }
        }
        Ok(out)
    }
}

fn parse_claude_jsonl(path: &Path) -> Option<Session> {
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
    let mut project = path
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut title: Option<String> = None;

    for v in &vals {
        // project from cwd or sessionId
        if let Some(cwd) = v.get("cwd").and_then(|c| c.as_str()) {
            // crude project from cwd last component
            if let Some(last) = cwd.rsplit('/').next() {
                if !last.is_empty() {
                    project = last.to_string();
                }
            }
        }
        // timestamp
        if let Some(ts) = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(|s| s.parse::<DateTime<Utc>>().ok())
        {
            updated_at = Some(updated_at.map_or(ts, |prev| prev.max(ts)));
        }
        // model: top-level or inside message
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

        // turn detection: type == "user" or "human"
        let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if typ == "user" || typ == "human" {
            turn_count += 1;
            // title from first user message
            if title.is_none() {
                if let Some(msg) = v.get("message") {
                    if let Some(content) = msg.get("content").and_then(|c| c.as_str()) {
                        title = Some(content.chars().take(80).collect());
                    } else if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                        for item in arr {
                            if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                                title = Some(text.chars().take(80).collect());
                                break;
                            }
                        }
                    }
                }
            }
        }

        // usage can be at top-level or inside message.usage or message.content
        let usage = v.get("usage").or_else(|| v.get("message").and_then(|m| m.get("usage")));
        if let Some(usage) = usage {
            tokens.input += usage
                .get("input_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
            tokens.output += usage
                .get("output_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
            tokens.cache_read += usage
                .get("cache_read_input_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
            tokens.cache_write += usage
                .get("cache_creation_input_tokens")
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
        }

        // tool_calls: look inside message.content array for type == "tool_use"
        let content_val = v.get("message").and_then(|m| m.get("content"));
        if let Some(arr) = content_val.and_then(|c| c.as_array()) {
            for c in arr {
                if c.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    tool_calls += 1;
                }
            }
        }
        // also direct content array
        if let Some(arr) = v.get("content").and_then(|c| c.as_array()) {
            for c in arr {
                if c.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                    tool_calls += 1;
                }
            }
        }
    }

    Some(Session {
        id,
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
        let cfg = InsightsConfig {
            sources: [("claude_code".to_string(), dir.path().to_path_buf())].into(),
            ..Default::default()
        };
        let a = ClaudeAdapter;
        assert!(a.scan(&cfg).unwrap().is_empty());
    }

    #[test]
    fn parses_user_and_tool_calls() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("test.jsonl");
        let content = r#"{"type":"user","timestamp":"2026-01-01T00:00:00Z","message":{"role":"user","content":[{"type":"text","text":"hello"}]}}
{"type":"assistant","timestamp":"2026-01-01T00:01:00Z","message":{"model":"claude-sonnet-4","usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":2},"content":[{"type":"tool_use","name":"Bash","input":{}}]}}
"#;
        std::fs::write(&file, content).unwrap();
        let cfg = InsightsConfig {
            sources: [("claude_code".to_string(), dir.path().to_path_buf())].into(),
            ..Default::default()
        };
        let sessions = ClaudeAdapter.scan(&cfg).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].turn_count, 1);
        assert_eq!(sessions[0].tool_call_count, 1);
        assert_eq!(sessions[0].tokens.input, 10);
    }
}
