// Adopted from .refs/agent-eval/src/rollout/readers/opencode-sqlite.ts
// and .refs/agenthud/docs/schemas/opencode-session.md
// and .refs/opencode/packages/core/src/session/sql.ts

use crate::config::InsightsConfig;
use crate::ingest::{
    common::{extract_file_path, extract_tokens, file_kind_for_tool, parse_timestamp},
    IngestBundle, IngestError, Adapter,
};
use crate::model::{Session, SessionSource, SessionStatus, TokenUsage, Turn, ToolCall};

pub struct OpenCodeAdapter;

impl Adapter for OpenCodeAdapter {
    fn source_name(&self) -> &'static str {
        "opencode"
    }
    fn scan(&self, config: &InsightsConfig) -> Result<IngestBundle, IngestError> {
        let Some(db_path) = config.sources.get("opencode") else {
            return Ok(IngestBundle::default());
        };
        if !db_path.exists() {
            return Ok(IngestBundle::default());
        }
        let conn = rusqlite::Connection::open(db_path)?;
        let mut bundle = IngestBundle::default();

        // Try current schema: session + message + part
        let sessions = try_session_table_dynamic(&conn);
        if sessions.is_empty() {
            // legacy fallback
            let legacy = try_sessions_table(&conn);
            bundle.sessions = legacy;
            return Ok(bundle);
        }

        for session in sessions {
            let (turns, tools, file_events) = ingest_messages_for_session(&conn, &session.id);
            let mut s = session;
            s.turn_count = turns.len() as u32;
            s.tool_call_count = tools.len() as u32;
            // aggregate tokens from turns if session tokens are 0
            if s.tokens.total() == 0 {
                let mut agg = TokenUsage {
                    input: 0,
                    output: 0,
                    cache_read: 0,
                    cache_write: 0,
                    reasoning: 0,
                };
                for t in &turns {
                    if let Some(tok) = &t.tokens {
                        agg.input += tok.input;
                        agg.output += tok.output;
                        agg.cache_read += tok.cache_read;
                        agg.cache_write += tok.cache_write;
                        agg.reasoning += tok.reasoning;
                    }
                }
                if agg.total() > 0 {
                    s.tokens = agg;
                }
            }
            bundle.turns.extend(turns);
            bundle.tool_calls.extend(tools);
            bundle.file_events.extend(file_events);
            bundle.sessions.push(s);
        }

        // DRY: subagents are child sessions (parent_id != null) - fetch via SQL
        if let Ok(mut stmt) = conn.prepare("SELECT id, parent_id FROM session WHERE parent_id IS NOT NULL LIMIT 1000") {
            if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                for row in rows.filter_map(|r| r.ok()) {
                    let (child_id, parent_id) = row;
                    bundle.subagents.push(crate::model::Subagent {
                        id: child_id.clone(),
                        session_id: parent_id.clone(),
                        parent_tool_call_id: None,
                        kind: "opencode_subagent".into(),
                        status: "completed".into(),
                        task: None,
                    });
                    // also increment subagent_count for parent
                    if let Some(parent) = bundle.sessions.iter_mut().find(|s| s.id == parent_id) {
                        parent.subagent_count += 1;
                    }
                }
            }
        }

        Ok(bundle)
    }
}

fn try_session_table_dynamic(conn: &rusqlite::Connection) -> Vec<Session> {
    let mut stmt = match conn.prepare("SELECT * FROM session LIMIT 1000") {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let find = |candidates: &[&str]| -> Option<usize> {
        for cand in candidates {
            if let Some(pos) = col_names.iter().position(|c| c == *cand) {
                return Some(pos);
            }
        }
        None
    };
    let id_idx = find(&["id"]).unwrap_or(0);
    let project_idx = find(&["project_id", "projectID", "project", "directory"]);
    let title_idx = find(&["title"]);
    let model_idx = find(&["model"]);
    let cost_idx = find(&["cost"]);
    let tokens_input_idx = find(&["tokens_input"]);
    let tokens_output_idx = find(&["tokens_output"]);
    let tokens_cache_read_idx = find(&["tokens_cache_read"]);
    let tokens_cache_write_idx = find(&["tokens_cache_write"]);
    let tokens_reasoning_idx = find(&["tokens_reasoning"]);
    let time_updated_idx = find(&["time_updated", "updated_at", "timeUpdated"]);
    let time_created_idx = find(&["time_created", "created_at", "timeCreated"]);

    let mut stmt2 = match conn.prepare("SELECT * FROM session LIMIT 1000") {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let mut rows = match stmt2.query([]) {
        Ok(r) => r,
        Err(_) => return vec![],
    };
    let mut out = Vec::new();
    while let Ok(Some(row)) = rows.next() {
        let id: String = row.get(id_idx).unwrap_or_else(|_| "unknown".to_string());
        let project: String = project_idx
            .and_then(|idx| row.get::<_, Option<String>>(idx).ok().flatten())
            .unwrap_or_else(|| "opencode".to_string());
        let title: Option<String> = title_idx
            .and_then(|idx| row.get::<_, Option<String>>(idx).ok().flatten());
        let model: Option<String> = model_idx
            .and_then(|idx| row.get::<_, Option<String>>(idx).ok().flatten())
            .and_then(|s| {
                // model is JSON {"id":"...","providerID":"..."}
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) {
                    v.get("id").and_then(|id| id.as_str()).map(|s| s.to_string())
                } else {
                    Some(s)
                }
            });
        let cost: Option<f64> = cost_idx
            .and_then(|idx| row.get::<_, Option<f64>>(idx).ok().flatten());
        let tokens = TokenUsage {
            input: tokens_input_idx
                .and_then(|idx| row.get::<_, Option<i64>>(idx).ok().flatten())
                .unwrap_or(0) as u64,
            output: tokens_output_idx
                .and_then(|idx| row.get::<_, Option<i64>>(idx).ok().flatten())
                .unwrap_or(0) as u64,
            cache_read: tokens_cache_read_idx
                .and_then(|idx| row.get::<_, Option<i64>>(idx).ok().flatten())
                .unwrap_or(0) as u64,
            cache_write: tokens_cache_write_idx
                .and_then(|idx| row.get::<_, Option<i64>>(idx).ok().flatten())
                .unwrap_or(0) as u64,
            reasoning: tokens_reasoning_idx
                .and_then(|idx| row.get::<_, Option<i64>>(idx).ok().flatten())
                .unwrap_or(0) as u64,
        };
        let updated_str: Option<String> = time_updated_idx
            .and_then(|idx| row.get::<_, Option<i64>>(idx).ok().flatten())
            .map(|ms| ms.to_string())
            .or_else(|| {
                time_updated_idx
                    .and_then(|idx| row.get::<_, Option<String>>(idx).ok().flatten())
            })
            .or_else(|| {
                time_created_idx
                    .and_then(|idx| row.get::<_, Option<i64>>(idx).ok().flatten())
                    .map(|ms| ms.to_string())
            });
        let updated_at = updated_str
            .as_deref()
            .and_then(|s| s.parse::<i64>().ok().and_then(|ms| chrono::DateTime::from_timestamp_millis(ms)))
            .or_else(|| {
                updated_str
                    .as_deref()
                    .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
            })
            .unwrap_or_else(chrono::Utc::now);

        // Try to resolve project name via project table
        let project_name = {
            let mut stmt = conn
                .prepare("SELECT name FROM project WHERE id = ?1")
                .ok();
            if let Some(stmt) = stmt.as_mut() {
                stmt.query_row([&project], |r| r.get::<_, String>(0))
                    .ok()
                    .unwrap_or(project.clone())
            } else {
                project.clone()
            }
        };

        out.push(Session {
            id,
            source: SessionSource::OpenCode,
            project: project_name,
            project_path: Some(project),
            title,
            model,
            status: SessionStatus::Completed,
            awaiting_reason: None,
            started_at: None,
            updated_at,
            ended_at: None,
            duration_secs: None,
            tokens,
            cost: cost.map(|total_usd| crate::model::Cost {
                total_usd,
                input_usd: 0.0,
                output_usd: 0.0,
                cache_read_usd: 0.0,
                cache_write_usd: 0.0,
            }),
            turn_count: 0,
            tool_call_count: 0,
            subagent_count: 0,
            tags: vec![],
            starred: false,
            pid: None,
            cwd: None,
        });
    }
    out
}

fn try_sessions_table(conn: &rusqlite::Connection) -> Vec<Session> {
    let mut out = Vec::new();
    let mut stmt = match conn.prepare(
        "SELECT id, project, model, updated_at, input_tokens, output_tokens, cost FROM sessions LIMIT 1000",
    ) {
        Ok(s) => s,
        Err(_) => return out,
    };
    let rows = match stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<i64>>(4)?,
            r.get::<_, Option<i64>>(5)?,
            r.get::<_, Option<f64>>(6)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(_) => return out,
    };
    for row in rows.filter_map(|r| r.ok()) {
        let (id, project, model, updated_at, input, output, cost) = row;
        let updated_at = updated_at
            .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
            .unwrap_or_else(chrono::Utc::now);
        out.push(Session {
            id,
            source: SessionSource::OpenCode,
            project: project.unwrap_or_else(|| "opencode".into()),
            project_path: None,
            title: None,
            model,
            status: SessionStatus::Completed,
            awaiting_reason: None,
            started_at: None,
            updated_at,
            ended_at: None,
            duration_secs: None,
            tokens: TokenUsage {
                input: input.unwrap_or(0) as u64,
                output: output.unwrap_or(0) as u64,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost: cost.map(|total_usd| crate::model::Cost {
                total_usd,
                input_usd: 0.0,
                output_usd: 0.0,
                cache_read_usd: 0.0,
                cache_write_usd: 0.0,
            }),
            turn_count: 0,
            tool_call_count: 0,
            subagent_count: 0,
            tags: vec![],
            starred: false,
            pid: None,
            cwd: None,
        });
    }
    out
}

fn ingest_messages_for_session(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> (Vec<Turn>, Vec<ToolCall>, Vec<crate::model::FileEvent>) {
    let mut turns = Vec::new();
    let mut tools = Vec::new();
    let mut file_events: Vec<crate::model::FileEvent> = Vec::new();

    let mut stmt = match conn.prepare(
        "SELECT id, data, time_created FROM message WHERE session_id = ?1 ORDER BY time_created, id",
    ) {
        Ok(s) => s,
        Err(_) => return (turns, tools, file_events),
    };
    let rows: Vec<(String, String, Option<i64>)> = match stmt.query_map([session_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
        ))
    }) {
        Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
        Err(_) => return (turns, tools, file_events),
    };

    let mut seq: u32 = 0;
    for (msg_id, data_str, time_created) in rows {
        let data: serde_json::Value = match serde_json::from_str(&data_str) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let role = data
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or("assistant");
        // Skip unknown roles but still count
        if role != "user" && role != "assistant" {
            continue;
        }
        seq += 1;
        let started_at = time_created
            .and_then(|ms| chrono::DateTime::from_timestamp_millis(ms))
            .or_else(|| {
                data.get("time")
                    .and_then(|t| t.get("created"))
                    .and_then(|v| parse_timestamp(v))
            });

        // tokens from message.data.tokens
        let tokens = data.get("tokens").map(extract_tokens);
        let cost_usd = data.get("cost").and_then(|v| v.as_f64());

        // Fetch parts for this message
        let parts = get_parts_for_message(conn, &msg_id);
        // DRY: split steps at step-start, group by step (from agent-eval)
        let steps = split_parts_into_steps(&parts);

        // For each step, create a Turn if it has content, and ToolCalls
        // Simplification: one Turn per message, aggregating all steps
        let mut user_text: Option<String> = None;
        let mut assistant_text: Option<String> = None;
        let mut reasoning_text = String::new();

        for step in &steps {
            for part in step {
                let typ = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
                match typ {
                    "text" => {
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if role == "user" {
                                let entry = user_text.get_or_insert_with(String::new);
                                if !entry.is_empty() {
                                    entry.push('\n');
                                }
                                entry.push_str(text);
                            } else {
                                let entry = assistant_text.get_or_insert_with(String::new);
                                if !entry.is_empty() {
                                    entry.push('\n');
                                }
                                entry.push_str(text);
                            }
                        }
                    }
                    "reasoning" => {
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if !reasoning_text.is_empty() {
                                reasoning_text.push('\n');
                            }
                            reasoning_text.push_str(text);
                        }
                    }
                    "tool" => {
                        let tool = part.get("tool").and_then(|t| t.as_str()).unwrap_or("unknown");
                        let call_id = part
                            .get("callID")
                            .or_else(|| part.get("callId"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let state = part.get("state");
                        let input = state
                            .and_then(|s| s.get("input"))
                            .cloned()
                            .unwrap_or(serde_json::Value::Null);
                        let output = state
                            .and_then(|s| s.get("output"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        let status = state
                            .and_then(|s| s.get("status"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("completed")
                            .to_string();
                        let created_at = state
                            .and_then(|s| s.get("time"))
                            .and_then(|t| t.get("start"))
                            .and_then(|v| parse_timestamp(v))
                            .or(started_at);
                        // Normalize tool_call id to be unique
                        let tool_id = if call_id == "unknown" {
                            format!("{}-{}-{}", session_id, seq, tools.len())
                        } else {
                            call_id.clone()
                        };
                        // DRY file_event
                        if let Some(path) = extract_file_path(tool, &input) {
                            file_events.push(crate::model::FileEvent {
                                id: format!("{}-fe-{}", session_id, file_events.len()),
                                session_id: session_id.to_string(),
                                path,
                                kind: file_kind_for_tool(tool).into(),
                                at: created_at.unwrap_or_else(chrono::Utc::now),
                            });
                        }
                        tools.push(ToolCall {
                            id: tool_id,
                            session_id: session_id.to_string(),
                            turn_seq: seq,
                            name: tool.to_string(),
                            input,
                            output,
                            status,
                            duration_ms: None,
                            created_at,
                        });
                    }
                    _ => {}
                }
            }
        }

        // If assistant and has reasoning, prepend to assistant_text
        if !reasoning_text.is_empty() && role == "assistant" {
            let entry = assistant_text.get_or_insert_with(String::new);
            if !entry.is_empty() {
                *entry = format!("{}\n\n{}", reasoning_text, entry);
            } else {
                *entry = reasoning_text.clone();
            }
        }

        let turn_id = format!("{}-{}", session_id, seq);
        // Only push turn if it has content or tools
        if user_text.is_some() || assistant_text.is_some() || !tools.is_empty() {
            turns.push(Turn {
                id: turn_id,
                session_id: session_id.to_string(),
                seq,
                user_text,
                assistant_text,
                started_at,
                ended_at: started_at,
                tokens,
                cost_usd,
            });
        }
    }

    (turns, tools, file_events)
}

fn get_parts_for_message(
    conn: &rusqlite::Connection,
    message_id: &str,
) -> Vec<serde_json::Value> {
    let mut stmt = match conn.prepare("SELECT data FROM part WHERE message_id = ?1 ORDER BY id") {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let rows = match stmt.query_map([message_id], |r| r.get::<_, String>(0)) {
        Ok(rows) => rows,
        Err(_) => return vec![],
    };
    let mut out = Vec::new();
    for row in rows.filter_map(|r| r.ok()) {
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&row) {
            out.push(v);
        }
    }
    out
}

fn split_parts_into_steps(parts: &[serde_json::Value]) -> Vec<Vec<serde_json::Value>> {
    // From agent-eval: split at step-start, skip step-finish/snapshot/patch
    let mut steps: Vec<Vec<serde_json::Value>> = Vec::new();
    let mut current: Vec<serde_json::Value> = Vec::new();
    for part in parts {
        let typ = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if typ == "step-start" {
            if !current.is_empty() {
                steps.push(std::mem::take(&mut current));
            }
            continue;
        }
        if typ == "step-finish" || typ == "snapshot" || typ == "patch" {
            continue;
        }
        current.push(part.clone());
    }
    if !current.is_empty() {
        steps.push(current);
    }
    // If no step-start at all, treat all parts as one step
    if steps.is_empty() && !parts.is_empty() {
        let filtered: Vec<serde_json::Value> = parts
            .iter()
            .filter(|p| {
                let t = p.get("type").and_then(|v| v.as_str()).unwrap_or("");
                t != "step-finish" && t != "snapshot" && t != "patch"
            })
            .cloned()
            .collect();
        if !filtered.is_empty() {
            steps.push(filtered);
        }
    }
    steps
}
