use crate::config::InsightsConfig;
use crate::ingest::{Adapter, IngestError};
use crate::model::{Session, SessionSource, SessionStatus, TokenUsage};

pub struct OpenCodeAdapter;

impl Adapter for OpenCodeAdapter {
    fn source_name(&self) -> &'static str { "opencode" }
    fn scan(&self, config: &InsightsConfig) -> Result<Vec<Session>, IngestError> {
        let Some(db_path) = config.sources.get("opencode") else { return Ok(vec![]); };
        if !db_path.exists() { return Ok(vec![]); }
        // OpenCode stores sessions in SQLite opencode.db
        let conn = rusqlite::Connection::open(db_path)?;
        // best-effort: if table missing, return empty (fail-open)
        let mut stmt = match conn.prepare("SELECT id, project, model, updated_at, input_tokens, output_tokens, cost FROM sessions LIMIT 1000") {
            Ok(s) => s,
            Err(_) => return Ok(vec![]),
        };
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, Option<i64>>(5)?,
                r.get::<_, Option<f64>>(6)?,
            ))
        })?;
        let mut out = Vec::new();
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
                cost: cost.map(|total_usd| crate::model::Cost { total_usd, input_usd: 0.0, output_usd: 0.0, cache_read_usd: 0.0, cache_write_usd: 0.0 }),
                turn_count: 0,
                tool_call_count: 0,
                subagent_count: 0,
                tags: vec![],
                starred: false,
                pid: None,
                cwd: None,
            });
        }
        Ok(out)
    }
}
