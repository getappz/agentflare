use crate::config::InsightsConfig;
use crate::ingest::{Adapter, IngestError};
use crate::model::{Session, SessionSource, SessionStatus, TokenUsage};

pub struct OpenCodeAdapter;

impl Adapter for OpenCodeAdapter {
    fn source_name(&self) -> &'static str {
        "opencode"
    }
    fn scan(&self, config: &InsightsConfig) -> Result<Vec<Session>, IngestError> {
        let Some(db_path) = config.sources.get("opencode") else {
            return Ok(vec![]);
        };
        if !db_path.exists() {
            return Ok(vec![]);
        }
        let conn = rusqlite::Connection::open(db_path)?;
        // try singular `session` first (current opencode), then plural `sessions` (legacy)
        let mut sessions = try_session_table_dynamic(&conn);
        if sessions.is_empty() {
            sessions = try_sessions_table(&conn);
        }
        Ok(sessions)
    }
}

fn try_session_table_dynamic(conn: &rusqlite::Connection) -> Vec<Session> {
    // Use dynamic column discovery: SELECT * and map by column name
    let mut stmt = match conn.prepare("SELECT * FROM session LIMIT 1000") {
        Ok(s) => s,
        Err(_) => return vec![],
    };
    let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    // Find indices
    let find = |name: &str| col_names.iter().position(|c| c == name);
    let find_any = |candidates: &[&str]| {
        candidates
            .iter()
            .find_map(|n| find(n))
    };
    let id_idx = find_any(&["id"]).unwrap_or(0);
    let project_idx = find_any(&["project_id", "projectID", "project", "directory"]);
    let model_idx = find_any(&["model", "model_id", "modelID"]);
    let created_idx = find_any(&["created_at", "createdAt", "created"]);
    let updated_idx = find_any(&["updated_at", "updatedAt", "updated", "time_updated"]);

    let mut out = Vec::new();
    let rows = match stmt.query([]) {
        Ok(r) => r,
        Err(_) => return out,
    };
    // Need to iterate via query_map with closure that uses indices
    // Re-prepare with SELECT * and then manually read via column index
    let mut stmt2 = match conn.prepare("SELECT * FROM session LIMIT 1000") {
        Ok(s) => s,
        Err(_) => return out,
    };
    let mut rows = match stmt2.query([]) {
        Ok(r) => r,
        Err(_) => return out,
    };
    while let Ok(Some(row)) = rows.next() {
        let id: String = row.get(id_idx).unwrap_or_else(|_| "unknown".to_string());
        let project: String = project_idx
            .and_then(|idx| row.get::<_, Option<String>>(idx).ok().flatten())
            .unwrap_or_else(|| "opencode".to_string());
        let model: Option<String> = model_idx
            .and_then(|idx| row.get::<_, Option<String>>(idx).ok().flatten());
        let updated_str: Option<String> = updated_idx
            .and_then(|idx| row.get::<_, Option<String>>(idx).ok().flatten())
            .or_else(|| {
                created_idx.and_then(|idx| row.get::<_, Option<String>>(idx).ok().flatten())
            });
        let updated_at = updated_str
            .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
            .unwrap_or_else(chrono::Utc::now);
        out.push(Session {
            id,
            source: SessionSource::OpenCode,
            project,
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
                input: 0,
                output: 0,
                cache_read: 0,
                cache_write: 0,
                reasoning: 0,
            },
            cost: None,
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
    let mut stmt = match conn
        .prepare("SELECT id, project, model, updated_at, input_tokens, output_tokens, cost FROM sessions LIMIT 1000")
    {
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
