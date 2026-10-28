use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use walkdir::WalkDir;

use crate::config::{InsightsConfig, PricingTable};
use crate::ingest::{
    common::{
        extract_file_path, extract_tokens, file_cursor, file_kind_for_tool, parse_timestamp,
        title_from_text,
    },
    Adapter, FileCursors, IngestBundle, IngestError,
};
use crate::model::{
    FileEvent, Session, SessionSource, SessionStatus, Subagent, TokenUsage, ToolCall, Turn,
};

pub struct AntigravityAdapter;

impl Adapter for AntigravityAdapter {
    fn source_name(&self) -> &'static str {
        "antigravity"
    }

    fn scan(
        &self,
        config: &InsightsConfig,
        cursors: &FileCursors,
        on_progress: &mut dyn FnMut(usize, usize),
    ) -> Result<IngestBundle, IngestError> {
        let Some(dir) = config.sources.get("antigravity") else {
            return Ok(IngestBundle::default());
        };
        if !dir.exists() {
            return Ok(IngestBundle::default());
        }

        let files: Vec<PathBuf> = WalkDir::new(dir)
            .max_depth(6)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy();
                name == "transcript.jsonl" || name.ends_with(".transcript.jsonl")
            })
            .map(|e| e.path().to_path_buf())
            .collect();

        let total = files.len();
        let mut bundle = IngestBundle::default();

        for (i, path) in files.iter().enumerate() {
            on_progress(i + 1, total);
            let Some(cursor) = file_cursor(path) else {
                continue;
            };
            if cursors.get(path) == Some(&cursor) {
                continue;
            }
            if let Some(b) = parse_antigravity_jsonl(path, &config.pricing) {
                bundle.sessions.extend(b.sessions);
                bundle.turns.extend(b.turns);
                bundle.tool_calls.extend(b.tool_calls);
                bundle.file_events.extend(b.file_events);
                bundle.subagents.extend(b.subagents);
            }
            bundle.file_cursors.push((path.clone(), cursor.0, cursor.1));
        }

        Ok(bundle)
    }
}

pub(crate) fn parse_antigravity_jsonl(path: &Path, pricing: &PricingTable) -> Option<IngestBundle> {
    let vals = crate::ingest::read_jsonl_sessions(path);
    if vals.is_empty() {
        return None;
    }

    // Antigravity logs are stored at <brain>/<conversation-id>/.system_generated/logs/transcript.jsonl
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let id = match file_name.strip_suffix(".transcript.jsonl") {
        Some(stem) if !stem.is_empty() => stem.to_string(),
        _ => path
            .parent()
            .and_then(|p| {
                if p.file_name().and_then(|n| n.to_str()) == Some("logs") {
                    p.parent()
                } else {
                    Some(p)
                }
            })
            .and_then(|p| {
                if p.file_name().and_then(|n| n.to_str()) == Some(".system_generated") {
                    p.parent()
                } else {
                    Some(p)
                }
            })
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string(),
    };

    let mut tokens = TokenUsage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        reasoning: 0,
    };

    let mut model: Option<String> = None;
    let mut started_at: Option<DateTime<Utc>> = None;
    let mut updated_at: Option<DateTime<Utc>> = None;
    let mut title: Option<String> = None;
    let mut project = "unknown".to_string();
    let mut project_path: Option<String> = None;

    let mut turns: Vec<Turn> = Vec::new();
    let mut tool_calls: Vec<ToolCall> = Vec::new();
    let mut file_events: Vec<FileEvent> = Vec::new();
    let mut subagents: Vec<Subagent> = Vec::new();
    let mut seq: u32 = 0;

    for v in &vals {
        let ts = v.get("created_at").and_then(parse_timestamp);
        if let Some(t) = ts {
            if started_at.is_none() {
                started_at = Some(t);
            }
            updated_at = Some(updated_at.map_or(t, |prev| prev.max(t)));
        }

        let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let source = v.get("source").and_then(|s| s.as_str()).unwrap_or("");

        // Model detection from settings changes or direct model field
        if let Some(m) = v.get("model").and_then(|m| m.as_str()) {
            model = Some(m.to_string());
        }

        if typ == "USER_INPUT" || source == "USER_EXPLICIT" {
            seq += 1;
            let raw_content = v.get("content").and_then(|c| c.as_str()).unwrap_or("");
            let user_text = extract_user_request(raw_content);

            if model.is_none() {
                if let Some(extracted_model) = extract_model_from_settings(raw_content) {
                    model = Some(extracted_model);
                }
            }

            if title.is_none() && !user_text.is_empty() {
                title = Some(title_from_text(&user_text, 80));
            }

            // Extract project path from environment hints if present
            if project == "unknown" {
                if let Some(ws) = extract_workspace_from_text(raw_content) {
                    project_path = Some(ws.clone());
                    if let Some(name) = Path::new(&ws).file_name().and_then(|n| n.to_str()) {
                        project = name.to_string();
                    }
                }
            }

            turns.push(Turn {
                id: format!("{}-{}", id, seq),
                session_id: id.clone(),
                seq,
                user_text: Some(user_text),
                assistant_text: None,
                started_at: ts,
                ended_at: ts,
                tokens: None,
                cost_usd: None,
            });
        } else if typ == "GENERIC" || typ == "TOOL_OUTPUT" {
            // Attach output to earliest pending tool call in current turn
            if let Some(last_tool) = tool_calls
                .iter_mut()
                .filter(|t| t.turn_seq == seq)
                .find(|t| t.output.is_none())
            {
                let content = v.get("content").and_then(|c| c.as_str());
                if let Some(text) = content {
                    last_tool.output = Some(text.to_string());
                    if text.contains("Command exited with code")
                        && !text.contains("Command exited with code 0")
                    {
                        last_tool.status = "failed".into();
                    }
                }
            }
        } else if typ == "PLANNER_RESPONSE" || source == "MODEL" {
            seq += 1;
            let assistant_text = v
                .get("thinking")
                .or_else(|| v.get("content"))
                .and_then(|c| c.as_str())
                .map(str::to_string);

            let t = extract_tokens(v);
            tokens.input += t.input;
            tokens.output += t.output;
            tokens.cache_read += t.cache_read;
            tokens.cache_write += t.cache_write;
            tokens.reasoning += t.reasoning;

            let turn_tokens = if t.input > 0 || t.output > 0 || t.cache_read > 0 {
                Some(t)
            } else {
                None
            };

            turns.push(Turn {
                id: format!("{}-{}", id, seq),
                session_id: id.clone(),
                seq,
                user_text: None,
                assistant_text,
                started_at: ts,
                ended_at: ts,
                tokens: turn_tokens,
                cost_usd: None,
            });

            // Process tool calls
            if let Some(calls) = v.get("tool_calls").and_then(|c| c.as_array()) {
                for call in calls {
                    let call_name = call
                        .get("name")
                        .and_then(|n| n.as_str())
                        .unwrap_or("unknown");
                    let call_args = call.get("args").cloned().unwrap_or(serde_json::Value::Null);
                    let call_id = format!("{}-{}-{}", id, seq, tool_calls.len());

                    // File events
                    if let Some(path_str) = extract_file_path(call_name, &call_args) {
                        file_events.push(FileEvent {
                            id: format!("{}-fe-{}", id, file_events.len()),
                            session_id: id.clone(),
                            path: path_str,
                            kind: file_kind_for_tool(call_name).into(),
                            at: ts.unwrap_or_else(Utc::now),
                        });
                    }

                    // Subagents
                    if call_name.contains("invoke_subagent") || call_name.contains("subagent") {
                        let task = call_args
                            .get("Prompt")
                            .or_else(|| call_args.get("prompt"))
                            .and_then(|p| p.as_str())
                            .map(str::to_string);
                        let kind = call_args
                            .get("Role")
                            .or_else(|| call_args.get("TypeName"))
                            .and_then(|r| r.as_str())
                            .unwrap_or(call_name)
                            .to_string();

                        subagents.push(Subagent {
                            id: call_id.clone(),
                            session_id: id.clone(),
                            parent_tool_call_id: Some(call_id.clone()),
                            kind,
                            status: "completed".into(),
                            task,
                        });
                    }

                    tool_calls.push(ToolCall {
                        id: call_id,
                        session_id: id.clone(),
                        turn_seq: seq,
                        name: call_name.to_string(),
                        input: call_args,
                        output: None,
                        status: "completed".into(),
                        duration_ms: None,
                        created_at: ts,
                    });
                }
            }
        }
    }

    let model_str = model.unwrap_or_else(|| "gemini".to_string());
    let cost = {
        let c = pricing.cost_for(Some(&model_str), &tokens);
        if c.total_usd > 0.0 {
            Some(c)
        } else {
            None
        }
    };

    let duration_secs = match (started_at, updated_at) {
        (Some(start), Some(end)) => u64::try_from((end - start).num_seconds().max(0)).ok(),
        _ => None,
    };

    let session = Session {
        id: id.clone(),
        source: SessionSource::Antigravity,
        project,
        project_path,
        title,
        model: Some(model_str),
        status: SessionStatus::Completed,
        awaiting_reason: None,
        started_at,
        updated_at: updated_at.unwrap_or_else(Utc::now),
        ended_at: updated_at,
        duration_secs,
        tokens,
        cost,
        turn_count: turns.len() as u32,
        tool_call_count: tool_calls.len() as u32,
        subagent_count: subagents.len() as u32,
        tags: vec![],
        starred: false,
        pid: None,
        cwd: None,
    };

    Some(IngestBundle {
        sessions: vec![session],
        turns,
        tool_calls,
        file_events,
        subagents,
        file_cursors: Vec::new(),
    })
}

fn extract_user_request(content: &str) -> String {
    if let Some(start) = content.find("<USER_REQUEST>") {
        let after = &content[start + "<USER_REQUEST>".len()..];
        if let Some(end) = after.find("</USER_REQUEST>") {
            return after[..end].trim().to_string();
        }
    }
    content.trim().to_string()
}

fn extract_model_from_settings(content: &str) -> Option<String> {
    if let Some(idx) = content.find("`Model Selection` from None to ") {
        let after = &content[idx + "`Model Selection` from None to ".len()..];
        if let Some(end) = after.find('.') {
            let m = after[..end].trim();
            return Some(m.to_lowercase().replace(' ', "-"));
        }
    }
    None
}

fn extract_workspace_from_text(content: &str) -> Option<String> {
    for prefix in [
        "Command Working Directory: ",
        "Active Workspaces:\n- ",
        "Active Workspaces: ",
    ] {
        if let Some(idx) = content.find(prefix) {
            let after = &content[idx + prefix.len()..];
            let line = after.lines().next().unwrap_or("").trim();
            if !line.is_empty() && line.starts_with('/') {
                return Some(line.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn empty_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let mut config = InsightsConfig::default();
        config
            .sources
            .insert("antigravity".into(), tmp.path().to_path_buf());
        let adapter = AntigravityAdapter;
        let cursors = FileCursors::new();
        let bundle = adapter.scan(&config, &cursors, &mut |_, _| {}).unwrap();
        assert!(bundle.sessions.is_empty());
    }

    #[test]
    fn parse_antigravity_transcript_extracts_sessions_turns_tools_and_tokens() {
        let tmp = TempDir::new().unwrap();
        let log_dir = tmp
            .path()
            .join("conv-123")
            .join(".system_generated")
            .join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();
        let transcript_path = log_dir.join("transcript.jsonl");

        let lines = [
            r#"{"step_index":0,"source":"USER_EXPLICIT","type":"USER_INPUT","status":"DONE","created_at":"2026-10-27T07:55:37Z","content":"<USER_REQUEST>\nbuild the widget\n</USER_REQUEST>\n<USER_SETTINGS_CHANGE>\nThe user changed setting `Model Selection` from None to Gemini 3.8 Flash (High).\n</USER_SETTINGS_CHANGE>"}"#,
            r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-10-27T07:55:38Z","input_tokens":100,"cache_read_tokens":50,"output_tokens":25,"thinking":"Analyzing...","tool_calls":[{"name":"read_file","args":{"Path":"src/main.rs"}}]}"#,
            r#"{"step_index":2,"source":"MODEL","type":"GENERIC","status":"DONE","created_at":"2026-10-27T07:55:39Z","content":"fn main() {}"}"#,
            r#"{"step_index":3,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-10-27T07:55:40Z","input_tokens":50,"output_tokens":10,"tool_calls":[{"name":"invoke_subagent","args":{"TypeName":"code-reviewer","Role":"Reviewer","Prompt":"Review widget"}}]}"#,
        ];

        let mut file = File::create(&transcript_path).unwrap();
        for line in &lines {
            writeln!(file, "{}", line).unwrap();
        }

        let bundle = parse_antigravity_jsonl(&transcript_path, &PricingTable::default())
            .expect("bundle parsed");
        assert_eq!(bundle.sessions.len(), 1);
        let session = &bundle.sessions[0];
        assert_eq!(session.id, "conv-123");
        assert_eq!(session.source, SessionSource::Antigravity);
        assert_eq!(session.title.as_deref(), Some("build the widget"));
        assert!(session.model.as_ref().unwrap().contains("gemini"));
        assert_eq!(session.tokens.input, 150);
        assert_eq!(session.tokens.cache_read, 50);
        assert_eq!(session.tokens.output, 35);

        assert_eq!(bundle.turns.len(), 3);
        assert_eq!(bundle.tool_calls.len(), 2);
        assert_eq!(bundle.file_events.len(), 1);
        assert_eq!(bundle.file_events[0].path, "src/main.rs");
        assert_eq!(bundle.file_events[0].kind, "read");
        assert_eq!(bundle.subagents.len(), 1);
        assert_eq!(bundle.subagents[0].kind, "Reviewer");
        assert_eq!(bundle.subagents[0].task.as_deref(), Some("Review widget"));

        // Verify *.transcript.jsonl stem derivation
        let stem_path = log_dir.join("sub-conv-456.transcript.jsonl");
        std::fs::copy(&transcript_path, &stem_path).unwrap();
        let stem_bundle =
            parse_antigravity_jsonl(&stem_path, &PricingTable::default()).expect("stem parsed");
        assert_eq!(stem_bundle.sessions[0].id, "sub-conv-456");
    }
}
