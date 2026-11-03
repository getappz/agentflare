use std::collections::HashMap;
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

        let metadata_cursors: Vec<_> = dir
            .parent()
            .into_iter()
            .flat_map(|root| {
                [
                    root.join("conversation_summaries.db"),
                    root.join("conversation_summaries.db-wal"),
                ]
            })
            // Persist absence too: cursors are upsert-only, so a removed WAL
            // must invalidate once without forcing every later scan to repeat.
            .map(|path| {
                let cursor = file_cursor(&path).unwrap_or_default();
                (path, cursor)
            })
            .collect();
        let metadata_changed = metadata_cursors
            .iter()
            .any(|(path, cursor)| cursors.get(path) != Some(cursor));
        let workspaces = metadata_cursors
            .first()
            .map(|(path, _)| native_workspaces(path))
            .unwrap_or_default();
        let total = files.len();
        let mut bundle = IngestBundle::default();

        for (i, path) in files.iter().enumerate() {
            on_progress(i + 1, total);
            let Some(cursor) = file_cursor(path) else {
                continue;
            };
            if cursors.get(path) == Some(&cursor) && !metadata_changed {
                continue;
            }
            if config.project.is_some() {
                let project = match workspaces.get(&transcript_id(path)) {
                    Some(workspace) => Some(workspace.to_string_lossy().into_owned()),
                    None => match super::common::jsonl_project(path) {
                        Ok(project) => project,
                        // Unreadable file: skip it instead of failing the scan.
                        Err(_) => continue,
                    },
                };
                if !config.matches_project(project.as_deref()) {
                    if project.is_some() {
                        bundle.file_cursors.push((path.clone(), cursor.0, cursor.1));
                    }
                    continue;
                }
            }
            if let Some(mut b) = parse_antigravity_jsonl(path, &config.pricing) {
                for session in &mut b.sessions {
                    if let Some(workspace) = workspaces
                        .get(&session.id)
                        .filter(|_| session.project_path.is_none())
                    {
                        session.project = workspace
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_else(|| workspace.to_string_lossy().into_owned());
                        session.project_path = Some(workspace.to_string_lossy().into_owned());
                    }
                }
                bundle.sessions.extend(b.sessions);
                bundle.turns.extend(b.turns);
                bundle.tool_calls.extend(b.tool_calls);
                bundle.file_events.extend(b.file_events);
                bundle.subagents.extend(b.subagents);
            }
            bundle.file_cursors.push((path.clone(), cursor.0, cursor.1));
        }

        bundle.file_cursors.extend(
            metadata_cursors
                .into_iter()
                .map(|(path, (mtime, size))| (path, mtime, size)),
        );
        Ok(bundle)
    }
}

/// Native transcripts may omit workspace hints; the summaries DB retains them.
/// Missing or changed schemas leave transcript ingestion available as before.
fn native_workspaces(db: &Path) -> HashMap<String, PathBuf> {
    let Ok(db) = std::path::absolute(db) else {
        return HashMap::new();
    };
    let Ok(mut uri) = url::Url::from_file_path(&db) else {
        return HashMap::new();
    };
    uri.query_pairs_mut().append_pair("mode", "ro");
    // A checkpointed WAL-mode DB otherwise gains empty WAL/SHM files even
    // through a read-only connection. Live WALs must still be read normally.
    if !db.with_extension("db-wal").exists() {
        uri.query_pairs_mut().append_pair("immutable", "1");
    }
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
        | rusqlite::OpenFlags::SQLITE_OPEN_URI;
    let Ok(conn) = rusqlite::Connection::open_with_flags(uri.as_str(), flags) else {
        return HashMap::new();
    };
    let Ok(mut stmt) =
        conn.prepare("SELECT conversation_id, workspace_uris FROM conversation_summaries")
    else {
        return HashMap::new();
    };
    let Ok(rows) = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    }) else {
        return HashMap::new();
    };
    rows.filter_map(Result::ok)
        .filter_map(|(id, raw)| {
            let uris: Vec<String> = serde_json::from_str(&raw).ok()?;
            let workspace = uris
                .into_iter()
                .find_map(|uri| url::Url::parse(&uri).ok()?.to_file_path().ok())?;
            Some((id, workspace.components().collect()))
        })
        .collect()
}

pub(crate) fn parse_antigravity_jsonl(path: &Path, pricing: &PricingTable) -> Option<IngestBundle> {
    let vals = crate::ingest::read_jsonl_sessions(path);
    if vals.is_empty() {
        return None;
    }

    let id = transcript_id(path);

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
                    // Antigravity stores tool args as JSON-encoded strings, so
                    // string values keep their surrounding quotes.
                    if let Some(path_str) = extract_file_path(call_name, &call_args)
                        .map(|p| p.trim_matches('"').to_string())
                    {
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

fn transcript_id(path: &Path) -> String {
    if let Some(stem) = path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|name| name.strip_suffix(".transcript.jsonl"))
        .filter(|s| !s.is_empty())
    {
        return stem.to_string();
    }
    // Native logs live at <conversation-id>/.system_generated/logs/transcript.jsonl.
    path.parent()
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
        .to_string()
}

pub(crate) fn extract_workspace_from_text(content: &str) -> Option<String> {
    for prefix in [
        "Command Working Directory: ",
        "Active Workspaces:\n- ",
        "Active Workspaces: ",
    ] {
        if let Some(idx) = content.find(prefix) {
            let after = &content[idx + prefix.len()..];
            let line = after.lines().next().unwrap_or("").trim();
            if line.starts_with('/') || Path::new(line).is_absolute() {
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

    #[test]
    fn antigravity_file_paths_drop_json_string_quotes() {
        let tmp = TempDir::new().unwrap();
        let logs = tmp.path().join("conv-q/.system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        let path = logs.join("transcript.jsonl");
        std::fs::write(
            &path,
            r#"{"step_index":1,"source":"MODEL","type":"PLANNER_RESPONSE","status":"DONE","created_at":"2026-10-27T07:55:38Z","tool_calls":[{"name":"read_file","args":{"Path":"\"src/main.rs\""}}]}"#,
        )
        .unwrap();
        let bundle = parse_antigravity_jsonl(&path, &PricingTable::default()).unwrap();
        assert_eq!(bundle.file_events[0].path, "src/main.rs");
    }

    #[test]
    fn antigravity_summary_metadata_restores_workspace_for_cached_transcripts() {
        let tmp = TempDir::new().unwrap();
        let brain = tmp.path().join("brain");
        let logs = brain.join("conv-123/.system_generated/logs");
        std::fs::create_dir_all(&logs).unwrap();
        std::fs::write(
            logs.join("transcript.jsonl"),
            r#"{"type":"USER_INPUT","source":"USER_EXPLICIT","created_at":"2026-10-27T07:55:37Z","content":"<USER_REQUEST>fix duplicate PR detection</USER_REQUEST>"}"#,
        ).unwrap();
        let mut config = InsightsConfig::default();
        config.sources.insert("antigravity".into(), brain);
        let adapter = AntigravityAdapter;
        let first = adapter
            .scan(&config, &Default::default(), &mut |_, _| {})
            .unwrap();
        assert!(first.sessions[0].project_path.is_none());
        let cached: FileCursors = first
            .file_cursors
            .into_iter()
            .map(|(path, mtime, size)| (path, (mtime, size)))
            .collect();

        let workspace = tmp.path().join("my project");
        let db = tmp.path().join("conversation_summaries.db");
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "CREATE TABLE conversation_summaries (conversation_id TEXT, workspace_uris TEXT);",
        )
        .unwrap();
        let uris = serde_json::to_string(&[url::Url::from_directory_path(&workspace)
            .unwrap()
            .to_string()])
        .unwrap();
        conn.execute(
            "INSERT INTO conversation_summaries VALUES (?1, ?2)",
            rusqlite::params!["conv-123", uris],
        )
        .unwrap();
        drop(conn);
        let before = std::fs::read(&db).unwrap();

        let refreshed = adapter.scan(&config, &cached, &mut |_, _| {}).unwrap();
        assert_eq!(
            refreshed.sessions.len(),
            1,
            "new metadata must refresh a cached transcript"
        );
        assert_eq!(
            refreshed.sessions[0].project_path.as_deref(),
            workspace.to_str()
        );
        assert_eq!(refreshed.sessions[0].project, "my project");
        assert_eq!(
            std::fs::read(&db).unwrap(),
            before,
            "metadata lookup must be read-only"
        );
        let cached: FileCursors = refreshed
            .file_cursors
            .into_iter()
            .map(|(path, mtime, size)| (path, (mtime, size)))
            .collect();
        assert!(adapter
            .scan(&config, &cached, &mut |_, _| {})
            .unwrap()
            .sessions
            .is_empty());

        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        conn.pragma_update(None, "wal_autocheckpoint", 0).unwrap();
        let updated_workspace = tmp.path().join("updated project");
        let uris = serde_json::to_string(&[url::Url::from_directory_path(&updated_workspace)
            .unwrap()
            .to_string()])
        .unwrap();
        conn.execute(
            "UPDATE conversation_summaries SET workspace_uris = ?1",
            rusqlite::params![uris],
        )
        .unwrap();
        let wal = tmp.path().join("conversation_summaries.db-wal");
        assert!(wal.exists());
        let updated = adapter.scan(&config, &cached, &mut |_, _| {}).unwrap();
        assert_eq!(
            updated.sessions[0].project_path.as_deref(),
            updated_workspace.to_str()
        );
        let cached: FileCursors = updated
            .file_cursors
            .into_iter()
            .map(|(path, mtime, size)| (path, (mtime, size)))
            .collect();
        drop(conn);
        assert!(!wal.exists());
        let checkpointed = adapter.scan(&config, &cached, &mut |_, _| {}).unwrap();
        assert!(!wal.exists(), "read-only lookup must not recreate a WAL");
        assert!(!db.with_extension("db-shm").exists());
        assert_eq!(
            checkpointed.sessions.len(),
            1,
            "WAL removal must refresh once"
        );
        let cached: FileCursors = checkpointed
            .file_cursors
            .into_iter()
            .map(|(path, mtime, size)| (path, (mtime, size)))
            .collect();
        assert!(adapter
            .scan(&config, &cached, &mut |_, _| {})
            .unwrap()
            .sessions
            .is_empty());

        std::fs::write(&db, b"unrecognized metadata format").unwrap();
        let fallback = adapter
            .scan(&config, &Default::default(), &mut |_, _| {})
            .unwrap();
        assert_eq!(fallback.sessions.len(), 1);
        assert!(fallback.sessions[0].project_path.is_none());
    }
}
