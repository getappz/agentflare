//! Cursor IDE session ingest from `state.vscdb` (`cursorDiskKV`).
//!
//! Keys:
//! - `composerData:<composerId>` — session metadata + ordered bubble headers
//! - `bubbleId:<composerId>:<bubbleId>` — user/assistant/tool/thinking payloads
//!
//! Spec source: agent-lens Cursor store layout + live `state.vscdb` samples.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

use crate::config::InsightsConfig;
use crate::config::PricingTable;
use crate::ingest::{
    common::{extract_file_path, file_cursor, file_kind_for_tool, parse_timestamp},
    Adapter, FileCursors, IngestBundle, IngestError,
};
use crate::model::{
    FileEvent, Session, SessionSource, SessionStatus, Subagent, TokenUsage, ToolCall, Turn,
};

pub struct CursorAdapter;

impl Adapter for CursorAdapter {
    fn source_name(&self) -> &'static str {
        "cursor"
    }

    fn scan(
        &self,
        config: &InsightsConfig,
        cursors: &FileCursors,
        on_progress: &mut dyn FnMut(usize, usize),
    ) -> Result<IngestBundle, IngestError> {
        let Some(db_path) = config.sources.get("cursor") else {
            on_progress(0, 0);
            return Ok(IngestBundle::default());
        };
        if !db_path.exists() {
            on_progress(0, 0);
            return Ok(IngestBundle::default());
        }

        // Single SQLite file — use the file-cursor skip cache like JSONL adapters.
        // Bypass while state.vscdb-wal is non-empty: SQLite can land new composer
        // rows in the WAL without changing the main DB mtime/size that file_cursor
        // tracks, so a cached skip would miss live Cursor writes.
        let Some(cursor) = file_cursor(db_path) else {
            on_progress(0, 0);
            return Ok(IngestBundle::default());
        };
        let wal_path = db_path.with_extension("vscdb-wal");
        let wal_nonempty = std::fs::metadata(&wal_path)
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false);
        if !wal_nonempty && cursors.get(db_path) == Some(&cursor) {
            on_progress(1, 1);
            return Ok(IngestBundle::default());
        }

        on_progress(0, 1);
        let (_keepalive, conn) = open_vscdb(db_path)?;
        let mut bundle = ingest_from_conn(&conn)?;
        bundle
            .file_cursors
            .push((db_path.clone(), cursor.0, cursor.1));
        on_progress(1, 1);
        Ok(bundle)
    }
}

/// Keep a temp copy alive when the live DB is locked by Cursor.
struct Keepalive(Option<PathBuf>);

impl Drop for Keepalive {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn open_vscdb(path: &Path) -> Result<(Keepalive, Connection), IngestError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    match Connection::open_with_flags(path, flags) {
        Ok(conn) => Ok((Keepalive(None), conn)),
        Err(_) => {
            let copy = std::env::temp_dir().join(format!(
                "flare-insights-cursor-{}-{}.vscdb",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::copy(path, &copy)?;
            let conn = Connection::open_with_flags(&copy, flags)?;
            Ok((Keepalive(Some(copy)), conn))
        }
    }
}

fn ingest_from_conn(conn: &Connection) -> Result<IngestBundle, IngestError> {
    let mut bundle = IngestBundle::default();
    let mut stmt = match conn
        .prepare("SELECT key, value FROM cursorDiskKV WHERE key LIKE 'composerData:%' ORDER BY key")
    {
        Ok(s) => s,
        // Missing table = nothing to ingest; other prepare failures must propagate
        // so scan() does not persist a file cursor as if the read succeeded.
        Err(e) if is_missing_cursor_disk_kv(&e) => return Ok(bundle),
        Err(e) => return Err(e.into()),
    };
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;

    for row in rows {
        let (key, value_bytes) = row?;
        let Some(composer_id) = key.strip_prefix("composerData:") else {
            continue;
        };
        if composer_id == "empty-state-draft" || composer_id.is_empty() {
            continue;
        }
        let text = match std::str::from_utf8(&value_bytes) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let data: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let headers = data
            .get("fullConversationHeadersOnly")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        if headers.is_empty() {
            continue;
        }
        if let Some(session_bundle) = ingest_composer(conn, composer_id, &data, &headers)? {
            bundle.sessions.push(session_bundle.session);
            bundle.turns.extend(session_bundle.turns);
            bundle.tool_calls.extend(session_bundle.tools);
            bundle.file_events.extend(session_bundle.files);
            bundle.subagents.extend(session_bundle.subagents);
        }
    }
    Ok(bundle)
}

fn is_missing_cursor_disk_kv(err: &rusqlite::Error) -> bool {
    match err {
        rusqlite::Error::SqliteFailure(_, Some(msg)) => msg.contains("no such table"),
        _ => err.to_string().contains("no such table"),
    }
}

struct SessionParts {
    session: Session,
    turns: Vec<Turn>,
    tools: Vec<ToolCall>,
    files: Vec<FileEvent>,
    subagents: Vec<Subagent>,
}

fn ingest_composer(
    conn: &Connection,
    composer_id: &str,
    data: &Value,
    headers: &[Value],
) -> Result<Option<SessionParts>, IngestError> {
    let mut turns: Vec<Turn> = Vec::new();
    let mut tools: Vec<ToolCall> = Vec::new();
    let mut files: Vec<FileEvent> = Vec::new();
    let mut tokens = TokenUsage {
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        reasoning: 0,
    };
    let mut seq: u32 = 0;
    let mut current_assistant = String::new();
    let mut current_user: Option<String> = None;
    let mut current_started: Option<DateTime<Utc>> = None;
    let mut current_ended: Option<DateTime<Utc>> = None;
    let mut open_turn = false;

    let flush_turn = |turns: &mut Vec<Turn>,
                      seq: &mut u32,
                      current_user: &mut Option<String>,
                      current_assistant: &mut String,
                      current_started: &mut Option<DateTime<Utc>>,
                      current_ended: &mut Option<DateTime<Utc>>,
                      open_turn: &mut bool| {
        if !*open_turn {
            return;
        }
        if current_user.is_none() && current_assistant.is_empty() {
            *open_turn = false;
            return;
        }
        *seq += 1;
        let assistant = if current_assistant.is_empty() {
            None
        } else {
            Some(std::mem::take(current_assistant))
        };
        turns.push(Turn {
            id: format!("{composer_id}-{}", *seq),
            session_id: composer_id.to_string(),
            seq: *seq,
            user_text: current_user.take(),
            assistant_text: assistant,
            started_at: *current_started,
            ended_at: current_ended.or(*current_started),
            tokens: None,
            cost_usd: None,
        });
        *current_started = None;
        *current_ended = None;
        *open_turn = false;
    };

    for header in headers {
        let bubble_id = header
            .get("bubbleId")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if bubble_id.is_empty() {
            continue;
        }
        let header_type = header.get("type").and_then(|v| v.as_i64()).unwrap_or(-1);
        let header_ts = header.get("createdAt").and_then(parse_timestamp);

        let bubble = load_bubble(conn, composer_id, bubble_id)?.unwrap_or(Value::Null);
        if let Some(tc) = bubble.get("tokenCount") {
            tokens.input += tc
                .get("inputTokens")
                .or_else(|| tc.get("input_tokens"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
            tokens.output += tc
                .get("outputTokens")
                .or_else(|| tc.get("output_tokens"))
                .and_then(|n| n.as_u64())
                .unwrap_or(0);
        }

        let bubble_type = bubble
            .get("type")
            .and_then(|v| v.as_i64())
            .unwrap_or(header_type);
        let capability = bubble
            .get("capabilityType")
            .and_then(|v| v.as_i64())
            .or_else(|| {
                header
                    .get("grouping")
                    .and_then(|g| g.get("capabilityType"))
                    .and_then(|v| v.as_i64())
            });

        // User message
        if bubble_type == 1 {
            flush_turn(
                &mut turns,
                &mut seq,
                &mut current_user,
                &mut current_assistant,
                &mut current_started,
                &mut current_ended,
                &mut open_turn,
            );
            let text = bubble
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string();
            current_user = Some(text);
            current_started = header_ts.or_else(|| bubble_created_at(&bubble));
            current_ended = current_started;
            open_turn = true;
            continue;
        }

        // Thinking bubbles — skip content (noisy for handoff); still keep turn open.
        if capability == Some(30) {
            if !open_turn {
                open_turn = true;
                current_started = header_ts.or_else(|| bubble_created_at(&bubble));
            }
            current_ended = header_ts
                .or_else(|| bubble_created_at(&bubble))
                .or(current_ended);
            continue;
        }

        // Tool call
        if capability == Some(15) || bubble.get("toolFormerData").is_some() {
            if !open_turn {
                open_turn = true;
                current_started = header_ts.or_else(|| bubble_created_at(&bubble));
            }
            let at = header_ts.or_else(|| bubble_created_at(&bubble));
            current_ended = at.or(current_ended);
            // In-progress turn will flush as `seq + 1`.
            if let Some(tool) = tool_from_bubble(composer_id, seq + 1, &bubble, at) {
                if let Some(path) = extract_file_path(&tool.name, &tool.input) {
                    files.push(FileEvent {
                        id: format!("{}-fe-{}", composer_id, files.len()),
                        session_id: composer_id.to_string(),
                        path,
                        kind: file_kind_for_tool(&tool.name).into(),
                        at: at.unwrap_or_else(Utc::now),
                    });
                }
                tools.push(tool);
            }
            continue;
        }

        // Assistant text (type 2, no tool/thinking)
        if bubble_type == 2 {
            let text = bubble
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .trim();
            if text.is_empty() {
                continue;
            }
            if !open_turn {
                open_turn = true;
                current_started = header_ts.or_else(|| bubble_created_at(&bubble));
            }
            if !current_assistant.is_empty() {
                current_assistant.push('\n');
            }
            current_assistant.push_str(text);
            current_ended = header_ts
                .or_else(|| bubble_created_at(&bubble))
                .or(current_ended);
        }
    }

    flush_turn(
        &mut turns,
        &mut seq,
        &mut current_user,
        &mut current_assistant,
        &mut current_started,
        &mut current_ended,
        &mut open_turn,
    );

    let max_seq = turns.last().map(|t| t.seq).unwrap_or(0);
    for tool in &mut tools {
        if tool.turn_seq == 0 || (max_seq > 0 && tool.turn_seq > max_seq) {
            tool.turn_seq = max_seq.max(1);
        }
    }

    if turns.is_empty() && tools.is_empty() {
        return Ok(None);
    }

    let cwd = workspace_fs_path(data);
    let project = cwd
        .as_deref()
        .and_then(|p| Path::new(p).file_name())
        .and_then(|n| n.to_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| "cursor".into());
    let title = data
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            turns
                .iter()
                .find_map(|t| t.user_text.as_deref())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
        })
        .map(|s| {
            let mut t = s.chars().take(80).collect::<String>();
            if s.chars().count() > 80 {
                t.push('\u{2026}');
            }
            t
        });
    let model = data
        .get("modelConfig")
        .and_then(|m| m.get("modelName"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());
    let created = data.get("createdAt").and_then(|v| {
        v.as_i64()
            .and_then(DateTime::from_timestamp_millis)
            .or_else(|| parse_timestamp(v))
    });
    let updated = data
        .get("lastUpdatedAt")
        .and_then(|v| {
            v.as_i64()
                .and_then(DateTime::from_timestamp_millis)
                .or_else(|| parse_timestamp(v))
        })
        .or(created)
        .or_else(|| turns.iter().rev().find_map(|t| t.ended_at.or(t.started_at)))
        .unwrap_or_else(Utc::now);
    let status = match data.get("status").and_then(|v| v.as_str()).unwrap_or("") {
        "completed" => SessionStatus::Completed,
        "aborted" | "cancelled" => SessionStatus::Abandoned,
        "generating" | "active" => SessionStatus::Active,
        _ => {
            if data
                .get("generatingBubbleIds")
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty())
            {
                SessionStatus::Active
            } else {
                SessionStatus::Completed
            }
        }
    };

    let mut subagents = Vec::new();
    if let Some(ids) = data.get("subagentComposerIds").and_then(|v| v.as_array()) {
        for idv in ids {
            if let Some(id) = idv.as_str() {
                subagents.push(Subagent {
                    id: id.to_string(),
                    session_id: composer_id.to_string(),
                    parent_tool_call_id: None,
                    kind: "cursor_subagent".into(),
                    status: "completed".into(),
                    task: None,
                });
            }
        }
    }

    let cost = {
        let pricing = PricingTable::default();
        let c = pricing.cost_for(model.as_deref(), &tokens);
        if c.total_usd > 0.0 {
            Some(c)
        } else {
            None
        }
    };
    let ended_at = if matches!(status, SessionStatus::Completed | SessionStatus::Abandoned) {
        Some(updated)
    } else {
        None
    };

    Ok(Some(SessionParts {
        session: Session {
            id: composer_id.to_string(),
            source: SessionSource::Cursor,
            project,
            project_path: cwd.clone(),
            title,
            model,
            status,
            awaiting_reason: None,
            started_at: created,
            updated_at: updated,
            ended_at,
            duration_secs: created.map(|c| (updated - c).num_seconds().max(0) as u64),
            tokens,
            cost,
            turn_count: turns.len() as u32,
            tool_call_count: tools.len() as u32,
            subagent_count: subagents.len() as u32,
            tags: vec![],
            starred: false,
            pid: None,
            cwd,
        },
        turns,
        tools,
        files,
        subagents,
    }))
}

fn load_bubble(
    conn: &Connection,
    composer_id: &str,
    bubble_id: &str,
) -> Result<Option<Value>, IngestError> {
    let key = format!("bubbleId:{composer_id}:{bubble_id}");
    let bytes: Vec<u8> = match conn.query_row(
        "SELECT value FROM cursorDiskKV WHERE key = ?1",
        [&key],
        |r| r.get(0),
    ) {
        Ok(bytes) => bytes,
        Err(rusqlite::Error::QueryReturnedNoRows) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(s) => s,
        Err(_) => return Ok(None),
    };
    Ok(serde_json::from_str(text).ok())
}

fn bubble_created_at(bubble: &Value) -> Option<DateTime<Utc>> {
    bubble
        .get("createdAt")
        .and_then(parse_timestamp)
        .or_else(|| {
            bubble
                .get("timingInfo")
                .and_then(|t| t.get("clientStartTime"))
                .and_then(parse_timestamp)
        })
}

fn workspace_fs_path(data: &Value) -> Option<String> {
    data.get("workspaceIdentifier")
        .and_then(|w| w.get("uri"))
        .and_then(|u| u.get("fsPath").or_else(|| u.get("path")))
        .and_then(|p| p.as_str())
        .map(|s| s.to_string())
}

fn tool_from_bubble(
    composer_id: &str,
    turn_seq: u32,
    bubble: &Value,
    at: Option<DateTime<Utc>>,
) -> Option<ToolCall> {
    let tfd = bubble.get("toolFormerData")?;
    let name = tfd
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .or_else(|| tfd.get("tool").and_then(|v| v.as_str()))
        .unwrap_or("unknown")
        .to_string();
    let call_id = tfd
        .get("toolCallId")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            // Multiple tool bubbles in one turn can share name+seq; include
            // bubbleId so fallback IDs stay unique across the session.
            let bid = bubble
                .get("bubbleId")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("{composer_id}-{turn_seq}-{name}-{bid}")
        });
    let input = parse_tool_input(tfd);
    let status = tfd
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "completed" | "success" => "completed",
            "cancelled" | "canceled" => "cancelled",
            "error" | "failed" => "failed",
            other => other,
        })
        .unwrap_or("completed")
        .to_string();
    let output = tfd
        .get("additionalData")
        .and_then(|a| {
            a.get("result")
                .or_else(|| a.get("output"))
                .or_else(|| a.get("error"))
        })
        .map(|v| {
            v.as_str()
                .map(str::to_string)
                .unwrap_or_else(|| v.to_string())
        });
    Some(ToolCall {
        id: call_id,
        session_id: composer_id.to_string(),
        turn_seq,
        name,
        input,
        output,
        status,
        duration_ms: None,
        created_at: at,
    })
}

fn parse_tool_input(tfd: &Value) -> Value {
    // Prefer structured params; fall back to rawArgs JSON string.
    if let Some(params) = tfd.get("params") {
        if let Some(s) = params.as_str() {
            if let Ok(v) = serde_json::from_str::<Value>(s) {
                return unwrap_tool_params(v);
            }
        } else if params.is_object() || params.is_array() {
            return unwrap_tool_params(params.clone());
        }
    }
    if let Some(raw) = tfd.get("rawArgs") {
        if let Some(s) = raw.as_str() {
            if let Ok(v) = serde_json::from_str::<Value>(s) {
                return unwrap_tool_params(v);
            }
            return Value::String(s.to_string());
        }
        if !raw.is_null() {
            return unwrap_tool_params(raw.clone());
        }
    }
    Value::Null
}

fn unwrap_tool_params(v: Value) -> Value {
    // Cursor MCP wrappers often nest `{tools:[{parameters:"{...json...}"}]}`.
    if let Some(tools) = v.get("tools").and_then(|t| t.as_array()) {
        if let Some(first) = tools.first() {
            if let Some(params) = first.get("parameters") {
                if let Some(s) = params.as_str() {
                    if let Ok(inner) = serde_json::from_str::<Value>(s) {
                        return inner;
                    }
                }
                return params.clone();
            }
        }
    }
    if let Some(args) = v.get("args") {
        return args.clone();
    }
    v
}

/// Default `state.vscdb` path for the current OS.
pub fn default_state_vscdb_path() -> PathBuf {
    // Windows: %APPDATA%\Cursor\...  (dirs::config_dir = Roaming)
    // macOS: ~/Library/Application Support/Cursor/...
    // Linux: ~/.config/Cursor/...
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("Cursor")
        .join("User")
        .join("globalStorage")
        .join("state.vscdb")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_fixture(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value BLOB);
             CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB);",
        )
        .unwrap();

        let composer_id = "comp-1";
        let composer = serde_json::json!({
            "_v": 18,
            "composerId": composer_id,
            "name": "Fix the adapter",
            "status": "completed",
            "createdAt": 1_700_000_000_000i64,
            "lastUpdatedAt": 1_700_000_100_000i64,
            "modelConfig": {"modelName": "gpt-5"},
            "workspaceIdentifier": {
                "id": "ws1",
                "uri": {"fsPath": "C:/Users/me/proj/my-app"}
            },
            "subagentComposerIds": [],
            "generatingBubbleIds": [],
            "fullConversationHeadersOnly": [
                {
                    "bubbleId": "b-user",
                    "type": 1,
                    "createdAt": "2023-11-14T22:13:20Z"
                },
                {
                    "bubbleId": "b-think",
                    "type": 2,
                    "grouping": {"capabilityType": 30},
                    "createdAt": "2023-11-14T22:13:21Z"
                },
                {
                    "bubbleId": "b-asst",
                    "type": 2,
                    "createdAt": "2023-11-14T22:13:22Z"
                },
                {
                    "bubbleId": "b-tool",
                    "type": 2,
                    "grouping": {"capabilityType": 15},
                    "createdAt": "2023-11-14T22:13:23Z"
                },
                {
                    "bubbleId": "b-asst2",
                    "type": 2,
                    "createdAt": "2023-11-14T22:13:24Z"
                }
            ]
        });
        let bubbles = [
            (
                "b-user",
                serde_json::json!({
                    "type": 1,
                    "bubbleId": "b-user",
                    "text": "please read Cargo.toml",
                    "tokenCount": {"inputTokens": 10, "outputTokens": 0}
                }),
            ),
            (
                "b-think",
                serde_json::json!({
                    "type": 2,
                    "capabilityType": 30,
                    "bubbleId": "b-think",
                    "thinking": {"text": "planning"},
                    "text": ""
                }),
            ),
            (
                "b-asst",
                serde_json::json!({
                    "type": 2,
                    "bubbleId": "b-asst",
                    "text": "Reading the manifest.\n",
                    "tokenCount": {"inputTokens": 0, "outputTokens": 5}
                }),
            ),
            (
                "b-tool",
                serde_json::json!({
                    "type": 2,
                    "capabilityType": 15,
                    "bubbleId": "b-tool",
                    "text": "",
                    "toolFormerData": {
                        "toolCallId": "call-1",
                        "name": "Read",
                        "status": "completed",
                        "rawArgs": "{\"path\":\"Cargo.toml\"}",
                        "params": {"path": "Cargo.toml"},
                        "additionalData": {"result": "[package]\nname = \"demo\""}
                    }
                }),
            ),
            (
                "b-asst2",
                serde_json::json!({
                    "type": 2,
                    "bubbleId": "b-asst2",
                    "text": "Done."
                }),
            ),
        ];

        conn.execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                format!("composerData:{composer_id}"),
                composer.to_string().into_bytes()
            ],
        )
        .unwrap();
        // Draft with no headers — must be skipped
        conn.execute(
            "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
            rusqlite::params![
                "composerData:empty-state-draft",
                serde_json::json!({
                    "composerId": "empty-state-draft",
                    "fullConversationHeadersOnly": [],
                    "status": "none"
                })
                .to_string()
                .into_bytes()
            ],
        )
        .unwrap();
        for (bid, body) in bubbles {
            conn.execute(
                "INSERT INTO cursorDiskKV(key, value) VALUES (?1, ?2)",
                rusqlite::params![
                    format!("bubbleId:{composer_id}:{bid}"),
                    body.to_string().into_bytes()
                ],
            )
            .unwrap();
        }
    }

    #[test]
    fn scans_composer_data_and_bubbles_into_session_turns_tools() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("state.vscdb");
        write_fixture(&db);
        let cfg = InsightsConfig {
            sources: [("cursor".into(), db.clone())].into(),
            ..Default::default()
        };
        let bundle = CursorAdapter
            .scan(&cfg, &FileCursors::default(), &mut |_, _| {})
            .unwrap();
        assert_eq!(bundle.sessions.len(), 1, "drafts skipped");
        let s = &bundle.sessions[0];
        assert_eq!(s.id, "comp-1");
        assert_eq!(s.source, SessionSource::Cursor);
        assert_eq!(s.title.as_deref(), Some("Fix the adapter"));
        assert_eq!(s.model.as_deref(), Some("gpt-5"));
        assert_eq!(s.project, "my-app");
        assert_eq!(s.cwd.as_deref(), Some("C:/Users/me/proj/my-app"));
        assert_eq!(s.status, SessionStatus::Completed);
        assert_eq!(bundle.turns.len(), 1);
        assert_eq!(
            bundle.turns[0].user_text.as_deref(),
            Some("please read Cargo.toml")
        );
        let asst = bundle.turns[0].assistant_text.as_deref().unwrap_or("");
        assert!(asst.contains("Reading the manifest"), "{asst}");
        assert!(asst.contains("Done."), "{asst}");
        assert!(!asst.contains("planning"), "thinking omitted: {asst}");
        assert_eq!(bundle.tool_calls.len(), 1);
        assert_eq!(bundle.tool_calls[0].name, "Read");
        assert_eq!(bundle.tool_calls[0].status, "completed");
        assert_eq!(
            bundle.tool_calls[0]
                .input
                .get("path")
                .and_then(|v| v.as_str()),
            Some("Cargo.toml")
        );
        assert!(
            bundle.tool_calls[0]
                .output
                .as_deref()
                .is_some_and(|o| o.contains("demo")),
            "{:?}",
            bundle.tool_calls[0].output
        );
        assert_eq!(bundle.file_events.len(), 1);
        assert!(bundle.file_events[0].path.contains("Cargo.toml"));
        assert!(s.tokens.input >= 10);
        assert!(s.tokens.output >= 5);
        assert_eq!(bundle.file_cursors.len(), 1);
        assert_eq!(bundle.file_cursors[0].0, db);
    }

    #[test]
    fn missing_db_returns_empty() {
        let dir = TempDir::new().unwrap();
        let cfg = InsightsConfig {
            sources: [("cursor".into(), dir.path().join("nope.vscdb"))].into(),
            ..Default::default()
        };
        let bundle = CursorAdapter
            .scan(&cfg, &FileCursors::default(), &mut |_, _| {})
            .unwrap();
        assert!(bundle.sessions.is_empty());
    }

    #[test]
    fn unchanged_file_cursor_skips_reparse() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("state.vscdb");
        write_fixture(&db);
        let cfg = InsightsConfig {
            sources: [("cursor".into(), db.clone())].into(),
            ..Default::default()
        };
        let cursor = file_cursor(&db).unwrap();
        let mut cursors = FileCursors::default();
        cursors.insert(db, cursor);
        let bundle = CursorAdapter.scan(&cfg, &cursors, &mut |_, _| {}).unwrap();
        assert!(bundle.sessions.is_empty());
        assert!(bundle.file_cursors.is_empty());
    }

    #[test]
    fn nonempty_wal_bypasses_file_cursor_skip() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("state.vscdb");
        write_fixture(&db);
        // Sibling WAL with bytes — main-file cursor is unchanged, but ingest must re-read.
        std::fs::write(db.with_extension("vscdb-wal"), b"wal-bytes").unwrap();
        let cfg = InsightsConfig {
            sources: [("cursor".into(), db.clone())].into(),
            ..Default::default()
        };
        let cursor = file_cursor(&db).unwrap();
        let mut cursors = FileCursors::default();
        cursors.insert(db, cursor);
        let bundle = CursorAdapter.scan(&cfg, &cursors, &mut |_, _| {}).unwrap();
        assert_eq!(bundle.sessions.len(), 1);
        assert_eq!(bundle.file_cursors.len(), 1);
    }

    #[test]
    fn missing_cursor_disk_kv_table_returns_empty_without_error() {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("state.vscdb");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value BLOB);")
            .unwrap();
        drop(conn);
        let cfg = InsightsConfig {
            sources: [("cursor".into(), db)].into(),
            ..Default::default()
        };
        let bundle = CursorAdapter
            .scan(&cfg, &FileCursors::default(), &mut |_, _| {})
            .unwrap();
        assert!(bundle.sessions.is_empty());
    }

    #[test]
    fn fallback_tool_call_id_includes_bubble_id() {
        let bubble_a = serde_json::json!({
            "bubbleId": "b-a",
            "toolFormerData": {"name": "Read", "params": {"path": "a.rs"}}
        });
        let bubble_b = serde_json::json!({
            "bubbleId": "b-b",
            "toolFormerData": {"name": "Read", "params": {"path": "b.rs"}}
        });
        let a = tool_from_bubble("comp", 1, &bubble_a, None).unwrap();
        let b = tool_from_bubble("comp", 1, &bubble_b, None).unwrap();
        assert_eq!(a.id, "comp-1-Read-b-a");
        assert_eq!(b.id, "comp-1-Read-b-b");
        assert_ne!(a.id, b.id);
    }
}
