use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

use crate::model::{Session, SessionSource, SessionStatus, TokenUsage, Turn, ToolCall};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct InsightsStore {
    conn: Connection,
}

impl InsightsStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        let s = Self { conn };
        s.migrate()?;
        Ok(s)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        let s = Self { conn };
        s.migrate()?;
        Ok(s)
    }

    fn migrate(&self) -> Result<(), StoreError> {
        self.conn.execute_batch(
            r#"
            PRAGMA journal_mode=WAL;
            PRAGMA foreign_keys=ON;

            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                source TEXT NOT NULL,
                project TEXT NOT NULL,
                project_path TEXT,
                title TEXT,
                model TEXT,
                status TEXT NOT NULL,
                awaiting_reason TEXT,
                started_at TEXT,
                updated_at TEXT NOT NULL,
                ended_at TEXT,
                duration_secs INTEGER,
                input_tokens INTEGER NOT NULL DEFAULT 0,
                output_tokens INTEGER NOT NULL DEFAULT 0,
                cache_read_tokens INTEGER NOT NULL DEFAULT 0,
                cache_write_tokens INTEGER NOT NULL DEFAULT 0,
                reasoning_tokens INTEGER NOT NULL DEFAULT 0,
                cost_usd REAL,
                turn_count INTEGER NOT NULL DEFAULT 0,
                tool_call_count INTEGER NOT NULL DEFAULT 0,
                subagent_count INTEGER NOT NULL DEFAULT 0,
                tags TEXT NOT NULL DEFAULT '[]',
                starred INTEGER NOT NULL DEFAULT 0,
                pid INTEGER,
                cwd TEXT
            );

            CREATE TABLE IF NOT EXISTS turns (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                seq INTEGER NOT NULL,
                user_text TEXT,
                assistant_text TEXT,
                started_at TEXT,
                ended_at TEXT,
                input_tokens INTEGER,
                output_tokens INTEGER,
                cost_usd REAL,
                UNIQUE(session_id, seq)
            );

            CREATE TABLE IF NOT EXISTS tool_calls (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                turn_seq INTEGER NOT NULL,
                name TEXT NOT NULL,
                input TEXT NOT NULL,
                output TEXT,
                status TEXT NOT NULL,
                duration_ms INTEGER,
                created_at TEXT
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS turns_fts USING fts5(
                session_id, user_text, assistant_text,
                content='turns', content_rowid='rowid',
                tokenize='trigram'
            );
            CREATE TRIGGER IF NOT EXISTS turns_ai AFTER INSERT ON turns BEGIN
                INSERT INTO turns_fts(rowid, session_id, user_text, assistant_text)
                VALUES (new.rowid, new.session_id, new.user_text, new.assistant_text);
            END;
            CREATE TRIGGER IF NOT EXISTS turns_ad AFTER DELETE ON turns BEGIN
                INSERT INTO turns_fts(turns_fts, rowid, session_id, user_text, assistant_text)
                VALUES('delete', old.rowid, old.session_id, old.user_text, old.assistant_text);
            END;
            CREATE TRIGGER IF NOT EXISTS turns_au AFTER UPDATE ON turns BEGIN
                INSERT INTO turns_fts(turns_fts, rowid, session_id, user_text, assistant_text)
                VALUES('delete', old.rowid, old.session_id, old.user_text, old.assistant_text);
                INSERT INTO turns_fts(rowid, session_id, user_text, assistant_text)
                VALUES (new.rowid, new.session_id, new.user_text, new.assistant_text);
            END;

            CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at);
            CREATE INDEX IF NOT EXISTS idx_sessions_project ON sessions(project);
            CREATE INDEX IF NOT EXISTS idx_sessions_source ON sessions(source);
            CREATE INDEX IF NOT EXISTS idx_turns_session ON turns(session_id);
            CREATE INDEX IF NOT EXISTS idx_tool_calls_session ON tool_calls(session_id);
            "#,
        )?;
        Ok(())
    }

    // ---- sessions ----

    pub fn upsert_session(&self, s: &Session) -> Result<(), StoreError> {
        let tags = serde_json::to_string(&s.tags)?;
        self.conn.execute(
            r#"INSERT INTO sessions(
                id,source,project,project_path,title,model,status,awaiting_reason,
                started_at,updated_at,ended_at,duration_secs,
                input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,reasoning_tokens,
                cost_usd,turn_count,tool_call_count,subagent_count,tags,starred,pid,cwd
            ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25)
            ON CONFLICT(id) DO UPDATE SET
                source=excluded.source, project=excluded.project, project_path=excluded.project_path,
                title=excluded.title, model=excluded.model, status=excluded.status,
                awaiting_reason=excluded.awaiting_reason, started_at=excluded.started_at,
                updated_at=excluded.updated_at, ended_at=excluded.ended_at, duration_secs=excluded.duration_secs,
                input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens,
                cache_read_tokens=excluded.cache_read_tokens, cache_write_tokens=excluded.cache_write_tokens,
                reasoning_tokens=excluded.reasoning_tokens, cost_usd=excluded.cost_usd,
                turn_count=excluded.turn_count, tool_call_count=excluded.tool_call_count,
                subagent_count=excluded.subagent_count, tags=excluded.tags, starred=excluded.starred,
                pid=excluded.pid, cwd=excluded.cwd
            "#,
            params![
                s.id,
                serde_json::to_string(&s.source).unwrap(),
                s.project,
                s.project_path,
                s.title,
                s.model,
                serde_json::to_string(&s.status).unwrap(),
                s.awaiting_reason.as_ref().map(|v| serde_json::to_string(v).unwrap()),
                s.started_at.map(|v| v.to_rfc3339()),
                s.updated_at.to_rfc3339(),
                s.ended_at.map(|v| v.to_rfc3339()),
                s.duration_secs.map(|v| v as i64),
                s.tokens.input as i64,
                s.tokens.output as i64,
                s.tokens.cache_read as i64,
                s.tokens.cache_write as i64,
                s.tokens.reasoning as i64,
                s.cost.as_ref().map(|c| c.total_usd),
                s.turn_count as i64,
                s.tool_call_count as i64,
                s.subagent_count as i64,
                tags,
                if s.starred { 1 } else { 0 },
                s.pid.map(|v| v as i64),
                s.cwd,
            ],
        )?;
        Ok(())
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT * FROM sessions WHERE id=?1")?;
        let row = stmt
            .query_row(params![id], |r| row_to_session(r))
            .optional()?;
        Ok(row)
    }

    pub fn list_sessions(&self, limit: usize, offset: usize) -> Result<Vec<Session>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM sessions ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], row_to_session)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn search_sessions(&self, query: &str, limit: usize) -> Result<Vec<Session>, StoreError> {
        let mut stmt = self.conn.prepare(
            r#"SELECT s.* FROM sessions s
               JOIN (SELECT session_id, rank FROM turns_fts WHERE turns_fts MATCH ?1 ORDER BY rank LIMIT ?2) f
               ON f.session_id = s.id
               ORDER BY s.updated_at DESC"#,
        )?;
        let rows = stmt.query_map(params![query, limit as i64], row_to_session)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn prune_older_than(&self, cutoff: DateTime<Utc>) -> Result<usize, StoreError> {
        let n = self.conn.execute(
            "DELETE FROM sessions WHERE updated_at < ?1",
            params![cutoff.to_rfc3339()],
        )?;
        Ok(n)
    }

    pub fn stats_totals(&self) -> Result<(u64, f64), StoreError> {
        let mut stmt = self.conn.prepare("SELECT COALESCE(SUM(input_tokens+output_tokens+cache_read_tokens+cache_write_tokens),0), COALESCE(SUM(cost_usd),0) FROM sessions")?;
        let (tokens, cost): (i64, f64) = stmt.query_row([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok((tokens as u64, cost))
    }

    // ---- turns (DRY batch) ----

    pub fn upsert_turn(&self, t: &Turn) -> Result<(), StoreError> {
        self.conn.execute(
            r#"INSERT INTO turns(id, session_id, seq, user_text, assistant_text, started_at, ended_at, input_tokens, output_tokens, cost_usd)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
               ON CONFLICT(id) DO UPDATE SET session_id=excluded.session_id, seq=excluded.seq, user_text=excluded.user_text, assistant_text=excluded.assistant_text, started_at=excluded.started_at, ended_at=excluded.ended_at, input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens, cost_usd=excluded.cost_usd"#,
            params![
                t.id,
                t.session_id,
                t.seq as i64,
                t.user_text,
                t.assistant_text,
                t.started_at.map(|v| v.to_rfc3339()),
                t.ended_at.map(|v| v.to_rfc3339()),
                t.tokens.as_ref().map(|tok| tok.input as i64),
                t.tokens.as_ref().map(|tok| tok.output as i64),
                t.cost_usd,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_turns_batch(&self, turns: &[Turn]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        for t in turns {
            tx.execute(
                r#"INSERT INTO turns(id, session_id, seq, user_text, assistant_text, started_at, ended_at, input_tokens, output_tokens, cost_usd)
                   VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                   ON CONFLICT(id) DO UPDATE SET user_text=excluded.user_text, assistant_text=excluded.assistant_text, input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens, cost_usd=excluded.cost_usd"#,
                params![
                    t.id,
                    t.session_id,
                    t.seq as i64,
                    t.user_text,
                    t.assistant_text,
                    t.started_at.map(|v| v.to_rfc3339()),
                    t.ended_at.map(|v| v.to_rfc3339()),
                    t.tokens.as_ref().map(|tok| tok.input as i64),
                    t.tokens.as_ref().map(|tok| tok.output as i64),
                    t.cost_usd,
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_turns(&self, session_id: &str) -> Result<Vec<Turn>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT * FROM turns WHERE session_id=?1 ORDER BY seq ASC")?;
        let rows = stmt.query_map(params![session_id], row_to_turn)?;
        let mut out = Vec::new();
        for r in rows { out.push(r?); }
        Ok(out)
    }

    // ---- tool_calls ----

    pub fn upsert_tool_call(&self, tc: &ToolCall) -> Result<(), StoreError> {
        self.conn.execute(
            r#"INSERT INTO tool_calls(id, session_id, turn_seq, name, input, output, status, duration_ms, created_at)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
               ON CONFLICT(id) DO UPDATE SET session_id=excluded.session_id, turn_seq=excluded.turn_seq, name=excluded.name, input=excluded.input, output=excluded.output, status=excluded.status, duration_ms=excluded.duration_ms, created_at=excluded.created_at"#,
            params![
                tc.id,
                tc.session_id,
                tc.turn_seq as i64,
                tc.name,
                serde_json::to_string(&tc.input).unwrap(),
                tc.output,
                tc.status,
                tc.duration_ms.map(|v| v as i64),
                tc.created_at.map(|v| v.to_rfc3339()),
            ],
        )?;
        Ok(())
    }

    pub fn upsert_tool_calls_batch(&self, tcs: &[ToolCall]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        for tc in tcs {
            tx.execute(
                r#"INSERT INTO tool_calls(id, session_id, turn_seq, name, input, output, status, duration_ms, created_at)
                   VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                   ON CONFLICT(id) DO UPDATE SET output=excluded.output, status=excluded.status, duration_ms=excluded.duration_ms"#,
                params![
                    tc.id,
                    tc.session_id,
                    tc.turn_seq as i64,
                    tc.name,
                    serde_json::to_string(&tc.input).unwrap(),
                    tc.output,
                    tc.status,
                    tc.duration_ms.map(|v| v as i64),
                    tc.created_at.map(|v| v.to_rfc3339()),
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_tool_calls(&self, session_id: &str) -> Result<Vec<ToolCall>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT * FROM tool_calls WHERE session_id=?1 ORDER BY turn_seq ASC")?;
        let rows = stmt.query_map(params![session_id], row_to_tool_call)?;
        let mut out = Vec::new();
        for r in rows { out.push(r?); }
        Ok(out)
    }

    /// DRY transactional session+turns+tools
    pub fn upsert_session_bundle(&self, s: &Session, turns: &[Turn], tools: &[ToolCall]) -> Result<(), StoreError> {
        self.upsert_session(s)?;
        if !turns.is_empty() { self.upsert_turns_batch(turns)?; }
        if !tools.is_empty() { self.upsert_tool_calls_batch(tools)?; }
        Ok(())
    }
}

fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
    let source_str: String = row.get("source")?;
    let status_str: String = row.get("status")?;
    let awaiting_str: Option<String> = row.get("awaiting_reason")?;
    let started: Option<String> = row.get("started_at")?;
    let updated: String = row.get("updated_at")?;
    let ended: Option<String> = row.get("ended_at")?;
    let tags_str: String = row.get("tags")?;
    let parse_dt = |s: String| s.parse::<DateTime<Utc>>().unwrap_or_else(|_| Utc::now());
    Ok(Session {
        id: row.get("id")?,
        source: serde_json::from_str(&source_str).unwrap_or(SessionSource::Unknown(source_str)),
        project: row.get("project")?,
        project_path: row.get("project_path")?,
        title: row.get("title")?,
        model: row.get("model")?,
        status: serde_json::from_str(&status_str).unwrap_or(SessionStatus::Idle),
        awaiting_reason: awaiting_str.and_then(|s| serde_json::from_str(&s).ok()),
        started_at: started.map(parse_dt),
        updated_at: parse_dt(updated),
        ended_at: ended.map(parse_dt),
        duration_secs: row.get::<_, Option<i64>>("duration_secs")?.map(|v| v as u64),
        tokens: TokenUsage {
            input: row.get::<_, i64>("input_tokens")? as u64,
            output: row.get::<_, i64>("output_tokens")? as u64,
            cache_read: row.get::<_, i64>("cache_read_tokens")? as u64,
            cache_write: row.get::<_, i64>("cache_write_tokens")? as u64,
            reasoning: row.get::<_, i64>("reasoning_tokens")? as u64,
        },
        cost: row.get::<_, Option<f64>>("cost_usd")?.map(|total_usd| crate::model::Cost {
            total_usd,
            input_usd: 0.0,
            output_usd: 0.0,
            cache_read_usd: 0.0,
            cache_write_usd: 0.0,
        }),
        turn_count: row.get::<_, i64>("turn_count")? as u32,
        tool_call_count: row.get::<_, i64>("tool_call_count")? as u32,
        subagent_count: row.get::<_, i64>("subagent_count")? as u32,
        tags: serde_json::from_str(&tags_str).unwrap_or_default(),
        starred: row.get::<_, i64>("starred")? != 0,
        pid: row.get::<_, Option<i64>>("pid")?.map(|v| v as u32),
        cwd: row.get("cwd")?,
    })
}

fn row_to_turn(row: &rusqlite::Row) -> rusqlite::Result<Turn> {
    let started: Option<String> = row.get("started_at")?;
    let ended: Option<String> = row.get("ended_at")?;
    let parse_dt = |s: String| s.parse::<DateTime<Utc>>().ok();
    let input: Option<i64> = row.get("input_tokens")?;
    let output: Option<i64> = row.get("output_tokens")?;
    let tokens = match (input, output) {
        (Some(i), Some(o)) => Some(TokenUsage { input: i as u64, output: o as u64, cache_read: 0, cache_write: 0, reasoning: 0 }),
        (Some(i), None) => Some(TokenUsage { input: i as u64, output: 0, cache_read: 0, cache_write: 0, reasoning: 0 }),
        _ => None,
    };
    Ok(Turn {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        seq: row.get::<_, i64>("seq")? as u32,
        user_text: row.get("user_text")?,
        assistant_text: row.get("assistant_text")?,
        started_at: started.and_then(parse_dt),
        ended_at: ended.and_then(parse_dt),
        tokens,
        cost_usd: row.get("cost_usd")?,
    })
}

fn row_to_tool_call(row: &rusqlite::Row) -> rusqlite::Result<ToolCall> {
    let input_str: String = row.get("input")?;
    let input = serde_json::from_str(&input_str).unwrap_or(serde_json::Value::Null);
    let created: Option<String> = row.get("created_at")?;
    Ok(ToolCall {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        turn_seq: row.get::<_, i64>("turn_seq")? as u32,
        name: row.get("name")?,
        input,
        output: row.get("output")?,
        status: row.get("status")?,
        duration_ms: row.get::<_, Option<i64>>("duration_ms")?.map(|v| v as u64),
        created_at: created.and_then(|s| s.parse::<DateTime<Utc>>().ok()),
    })
}
