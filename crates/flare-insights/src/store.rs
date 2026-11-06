use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::model::{Session, SessionSource, SessionStatus, TokenUsage, ToolCall, Turn};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("migration: {0}")]
    Migration(#[from] rusqlite_migration::Error),
}

pub struct InsightsStore {
    pub(crate) conn: Connection,
}

impl InsightsStore {
    /// Inspect an existing index without creating it or running migrations.
    pub fn open_read_only(path: &Path) -> Result<Self, StoreError> {
        Ok(Self {
            conn: Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?,
        })
    }

    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let mut conn = Connection::open_in_memory()?;
        migrate(&mut conn)?;
        Ok(Self { conn })
    }

    // ---- sessions ----

    pub fn upsert_session(&self, s: &Session) -> Result<(), StoreError> {
        exec_upsert_session(&self.conn, s)
    }

    /// Batched, single-transaction session upsert. Prefer this over calling
    /// `upsert_session` in a loop -- one commit/fsync per session is the
    /// dominant cost of a large sync, unlike turns/tool_calls/file_events
    /// which already batch into one transaction.
    pub fn upsert_sessions_batch(&self, sessions: &[Session]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        for s in sessions {
            exec_upsert_session(&tx, s)?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_session(&self, id: &str) -> Result<Option<Session>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT * FROM sessions WHERE id=?1")?;
        let row = stmt.query_row(params![id], row_to_session).optional()?;
        Ok(row)
    }

    /// Codex indexes historically used rollout filenames instead of provider UUIDs.
    pub fn get_session_by_provider_id(&self, id: &str) -> Result<Option<Session>, StoreError> {
        if let Some(session) = self.get_session(id)? {
            return Ok(Some(session));
        }
        if uuid::Uuid::parse_str(id).is_err() {
            return Ok(None);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM sessions WHERE source = ?1 AND id GLOB ?2 LIMIT 2")?;
        let mut sessions = stmt.query_map(
            params!["\"codex\"", format!("rollout-*-{id}")],
            row_to_session,
        )?;
        let first = sessions.next().transpose()?;
        // An ambiguous native ID must not attach another session's evidence.
        if sessions.next().transpose()?.is_some() {
            return Ok(None);
        }
        Ok(first)
    }

    pub fn list_sessions(&self, limit: usize, offset: usize) -> Result<Vec<Session>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM sessions ORDER BY updated_at DESC LIMIT ?1 OFFSET ?2")?;
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

    pub fn search_sessions_by_file_path(
        &self,
        pattern: &str,
        limit: usize,
    ) -> Result<Vec<Session>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT s.* FROM sessions s JOIN file_events f ON f.session_id = s.id WHERE f.path LIKE ?1 ORDER BY s.updated_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], row_to_session)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn search_sessions_by_tool(
        &self,
        pattern: &str,
        limit: usize,
    ) -> Result<Vec<Session>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT s.* FROM sessions s JOIN tool_calls t ON t.session_id = s.id WHERE t.name LIKE ?1 ORDER BY s.updated_at DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![pattern, limit as i64], row_to_session)?;
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
            r#"INSERT INTO turns(id, session_id, seq, user_text, assistant_text, started_at, ended_at, input_tokens, output_tokens, cost_usd, cache_read_tokens, cache_write_tokens, reasoning_tokens)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
               ON CONFLICT(id) DO UPDATE SET session_id=excluded.session_id, seq=excluded.seq, user_text=excluded.user_text, assistant_text=excluded.assistant_text, started_at=excluded.started_at, ended_at=excluded.ended_at, input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens, cost_usd=excluded.cost_usd, cache_read_tokens=excluded.cache_read_tokens, cache_write_tokens=excluded.cache_write_tokens, reasoning_tokens=excluded.reasoning_tokens"#,
            rusqlite::params_from_iter(turn_params(t)),
        )?;
        Ok(())
    }

    pub fn upsert_turns_batch(&self, turns: &[Turn]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        for t in turns {
            tx.execute(
                r#"INSERT INTO turns(id, session_id, seq, user_text, assistant_text, started_at, ended_at, input_tokens, output_tokens, cost_usd, cache_read_tokens, cache_write_tokens, reasoning_tokens)
                   VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
                   ON CONFLICT(id) DO UPDATE SET session_id=excluded.session_id, seq=excluded.seq, user_text=excluded.user_text, assistant_text=excluded.assistant_text, started_at=excluded.started_at, ended_at=excluded.ended_at, input_tokens=excluded.input_tokens, output_tokens=excluded.output_tokens, cost_usd=excluded.cost_usd, cache_read_tokens=excluded.cache_read_tokens, cache_write_tokens=excluded.cache_write_tokens, reasoning_tokens=excluded.reasoning_tokens"#,
                rusqlite::params_from_iter(turn_params(t)),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_turns(&self, session_id: &str) -> Result<Vec<Turn>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM turns WHERE session_id=?1 ORDER BY seq ASC")?;
        let rows = stmt.query_map(params![session_id], row_to_turn)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ---- tool_calls ----

    pub fn upsert_tool_call(&self, tc: &ToolCall) -> Result<(), StoreError> {
        self.conn.execute(
            r#"INSERT INTO tool_calls(id, session_id, turn_seq, name, input, output, status, duration_ms, created_at)
               VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
               ON CONFLICT(id) DO UPDATE SET session_id=excluded.session_id, turn_seq=excluded.turn_seq, name=excluded.name, input=excluded.input, output=excluded.output, status=excluded.status, duration_ms=excluded.duration_ms, created_at=excluded.created_at"#,
            rusqlite::params_from_iter(tool_call_params(tc)),
        )?;
        Ok(())
    }

    pub fn upsert_tool_calls_batch(&self, tcs: &[ToolCall]) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        for tc in tcs {
            tx.execute(
                r#"INSERT INTO tool_calls(id, session_id, turn_seq, name, input, output, status, duration_ms, created_at)
                   VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)
                    ON CONFLICT(id) DO UPDATE SET session_id=excluded.session_id, turn_seq=excluded.turn_seq, name=excluded.name, input=excluded.input, output=excluded.output, status=excluded.status, duration_ms=excluded.duration_ms, created_at=excluded.created_at"#,
                rusqlite::params_from_iter(tool_call_params(tc)),
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_tool_calls(&self, session_id: &str) -> Result<Vec<ToolCall>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM tool_calls WHERE session_id=?1 ORDER BY turn_seq ASC")?;
        let rows = stmt.query_map(params![session_id], row_to_tool_call)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn upsert_file_events_batch(
        &self,
        events: &[crate::model::FileEvent],
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        for e in events {
            tx.execute(
                "INSERT INTO file_events(id, session_id, path, kind, at) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET path=excluded.path, kind=excluded.kind, at=excluded.at",
                params![e.id, e.session_id, e.path, e.kind, e.at.to_rfc3339()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_file_events(
        &self,
        session_id: &str,
    ) -> Result<Vec<crate::model::FileEvent>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM file_events WHERE session_id=?1 ORDER BY at ASC")?;
        let rows = stmt.query_map(params![session_id], row_to_file_event)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn upsert_subagents_batch(
        &self,
        subs: &[crate::model::Subagent],
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        for s in subs {
            tx.execute(
                "INSERT INTO subagents(id, session_id, parent_tool_call_id, kind, status, task) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(id) DO UPDATE SET kind=excluded.kind, status=excluded.status, task=excluded.task",
                params![s.id, s.session_id, s.parent_tool_call_id, s.kind, s.status, s.task],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_subagents(
        &self,
        session_id: &str,
    ) -> Result<Vec<crate::model::Subagent>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM subagents WHERE session_id=?1")?;
        let rows = stmt.query_map(params![session_id], row_to_subagent)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn list_tool_calls(&self, limit: usize) -> Result<Vec<ToolCall>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT * FROM tool_calls LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], row_to_tool_call)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn list_file_events(
        &self,
        limit: usize,
    ) -> Result<Vec<crate::model::FileEvent>, StoreError> {
        let mut stmt = self.conn.prepare("SELECT * FROM file_events LIMIT ?1")?;
        let rows = stmt.query_map(params![limit as i64], row_to_file_event)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    // ---- ingest file cursors (skip-unchanged-file cache for `sync`) ----

    pub fn load_file_cursors(
        &self,
        source: &str,
    ) -> Result<HashMap<PathBuf, (i64, u64)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT path, mtime_ms, size FROM ingest_file_cursors WHERE source=?1")?;
        let rows = stmt.query_map(params![source], |r| {
            let path: String = r.get(0)?;
            let mtime_ms: i64 = r.get(1)?;
            let size: i64 = r.get(2)?;
            Ok((PathBuf::from(path), (mtime_ms, size as u64)))
        })?;
        let mut out = HashMap::new();
        for r in rows {
            let (path, cursor) = r?;
            out.insert(path, cursor);
        }
        Ok(out)
    }

    pub fn upsert_file_cursors_batch(
        &self,
        source: &str,
        entries: &[(PathBuf, i64, u64)],
    ) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        for (path, mtime_ms, size) in entries {
            exec_upsert_file_cursor(&tx, source, path, *mtime_ms, *size)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Commit one source's scan atomically: every session in the bundle has
    /// its children replaced (delete + plain insert, so a rewritten file with
    /// shifted `seq` numbers cannot collide), then the file cursors advance.
    /// Any failure rolls the whole bundle back, cursors included, so the
    /// files are re-read on the next sync instead of hidden until `--force`.
    pub fn commit_ingest(
        &self,
        source: &str,
        bundle: &crate::ingest::IngestBundle,
    ) -> Result<IngestStats, StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        let mut stats = IngestStats::default();
        for s in &bundle.sessions {
            for table in ["turns", "tool_calls", "file_events", "subagents"] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE session_id=?1"),
                    params![s.id],
                )?;
            }
            exec_upsert_session(&tx, s)?;
            stats.sessions += 1;
        }
        for t in &bundle.turns {
            stats.turns += tx.execute(
                "INSERT INTO turns(id, session_id, seq, user_text, assistant_text, started_at, ended_at, input_tokens, output_tokens, cost_usd, cache_read_tokens, cache_write_tokens, reasoning_tokens)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
                rusqlite::params_from_iter(turn_params(t)),
            )?;
        }
        for tc in &bundle.tool_calls {
            stats.tool_calls += tx.execute(
                "INSERT INTO tool_calls(id, session_id, turn_seq, name, input, output, status, duration_ms, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                rusqlite::params_from_iter(tool_call_params(tc)),
            )?;
        }
        for e in &bundle.file_events {
            stats.file_events += tx.execute(
                "INSERT INTO file_events(id, session_id, path, kind, at) VALUES (?1,?2,?3,?4,?5)",
                params![e.id, e.session_id, e.path, e.kind, e.at.to_rfc3339()],
            )?;
        }
        for s in &bundle.subagents {
            stats.subagents += tx.execute(
                "INSERT INTO subagents(id, session_id, parent_tool_call_id, kind, status, task) VALUES (?1,?2,?3,?4,?5,?6)",
                params![s.id, s.session_id, s.parent_tool_call_id, s.kind, s.status, s.task],
            )?;
        }
        for (path, mtime_ms, size) in &bundle.file_cursors {
            exec_upsert_file_cursor(&tx, source, path, *mtime_ms, *size)?;
        }
        tx.commit()?;
        Ok(stats)
    }
}

/// Rows written by one [`InsightsStore::commit_ingest`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct IngestStats {
    pub sessions: usize,
    pub turns: usize,
    pub tool_calls: usize,
    pub file_events: usize,
    pub subagents: usize,
}

impl std::ops::AddAssign for IngestStats {
    fn add_assign(&mut self, o: Self) {
        self.sessions += o.sessions;
        self.turns += o.turns;
        self.tool_calls += o.tool_calls;
        self.file_events += o.file_events;
        self.subagents += o.subagents;
    }
}

fn exec_upsert_file_cursor(
    conn: &Connection,
    source: &str,
    path: &Path,
    mtime_ms: i64,
    size: u64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO ingest_file_cursors(source, path, mtime_ms, size) VALUES (?1,?2,?3,?4)
         ON CONFLICT(source, path) DO UPDATE SET mtime_ms=excluded.mtime_ms, size=excluded.size",
        params![source, path.to_string_lossy(), mtime_ms, size as i64],
    )?;
    Ok(())
}

/// Positional params shared by the tool_call upserts and `commit_ingest` (`?1..?9`).
fn tool_call_params(tc: &ToolCall) -> [Box<dyn rusqlite::ToSql + '_>; 9] {
    [
        Box::new(&tc.id),
        Box::new(&tc.session_id),
        Box::new(tc.turn_seq as i64),
        Box::new(&tc.name),
        Box::new(serde_json::to_string(&tc.input).unwrap()),
        Box::new(&tc.output),
        Box::new(&tc.status),
        Box::new(tc.duration_ms.map(|v| v as i64)),
        Box::new(tc.created_at.map(|v| v.to_rfc3339())),
    ]
}

fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
    crate::migrations::migrations().to_latest(conn)?;
    Ok(())
}

/// Positional params shared by the single and batch turn upserts (`?1..?13`).
fn turn_params(t: &Turn) -> [Box<dyn rusqlite::ToSql + '_>; 13] {
    let tok = t.tokens.as_ref();
    [
        Box::new(&t.id),
        Box::new(&t.session_id),
        Box::new(t.seq as i64),
        Box::new(&t.user_text),
        Box::new(&t.assistant_text),
        Box::new(t.started_at.map(|v| v.to_rfc3339())),
        Box::new(t.ended_at.map(|v| v.to_rfc3339())),
        Box::new(tok.map(|v| v.input as i64)),
        Box::new(tok.map(|v| v.output as i64)),
        Box::new(t.cost_usd),
        Box::new(tok.map(|v| v.cache_read as i64)),
        Box::new(tok.map(|v| v.cache_write as i64)),
        Box::new(tok.map(|v| v.reasoning as i64)),
    ]
}

fn exec_upsert_session(conn: &Connection, s: &Session) -> Result<(), StoreError> {
    let tags = serde_json::to_string(&s.tags)?;
    conn.execute(
        r#"INSERT INTO sessions(
            id,source,project,project_path,title,model,status,awaiting_reason,
            started_at,updated_at,ended_at,duration_secs,
            input_tokens,output_tokens,cache_read_tokens,cache_write_tokens,reasoning_tokens,
            cost_usd,turn_count,tool_call_count,subagent_count,tags,starred,pid,cwd,cost_basis,pricing_key
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22,?23,?24,?25,?26,?27)
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
            pid=excluded.pid, cwd=excluded.cwd, cost_basis=excluded.cost_basis, pricing_key=excluded.pricing_key
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
            s.cost_basis.as_str(),
            s.pricing_key,
        ],
    )?;
    Ok(())
}

pub(crate) fn row_to_session(row: &rusqlite::Row) -> rusqlite::Result<Session> {
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
        duration_secs: row
            .get::<_, Option<i64>>("duration_secs")?
            .map(|v| v as u64),
        tokens: TokenUsage {
            input: row.get::<_, i64>("input_tokens")? as u64,
            output: row.get::<_, i64>("output_tokens")? as u64,
            cache_read: row.get::<_, i64>("cache_read_tokens")? as u64,
            cache_write: row.get::<_, i64>("cache_write_tokens")? as u64,
            reasoning: row.get::<_, i64>("reasoning_tokens")? as u64,
        },
        cost: row
            .get::<_, Option<f64>>("cost_usd")?
            .map(|total_usd| crate::model::Cost {
                total_usd,
                input_usd: 0.0,
                output_usd: 0.0,
                cache_read_usd: 0.0,
                cache_write_usd: 0.0,
            }),
        cost_basis: match row.get::<_, Option<String>>("cost_basis")?.as_deref() {
            Some("provider") => crate::model::CostBasis::Provider,
            Some("estimated") => crate::model::CostBasis::Estimated,
            _ => crate::model::CostBasis::Unknown,
        },
        pricing_key: row.get("pricing_key")?,
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
    let col = |name: &str| {
        row.get::<_, Option<i64>>(name)
            .map(|v| v.unwrap_or(0) as u64)
    };
    let tokens = match row.get::<_, Option<i64>>("input_tokens")? {
        Some(i) => Some(TokenUsage {
            input: i as u64,
            output: col("output_tokens")?,
            cache_read: col("cache_read_tokens")?,
            cache_write: col("cache_write_tokens")?,
            reasoning: col("reasoning_tokens")?,
        }),
        None => None,
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

fn row_to_file_event(row: &rusqlite::Row) -> rusqlite::Result<crate::model::FileEvent> {
    let at_str: String = row.get("at")?;
    let at = at_str
        .parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| Utc::now());
    Ok(crate::model::FileEvent {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        path: row.get("path")?,
        kind: row.get("kind")?,
        at,
    })
}

fn row_to_subagent(row: &rusqlite::Row) -> rusqlite::Result<crate::model::Subagent> {
    Ok(crate::model::Subagent {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        parent_tool_call_id: row.get("parent_tool_call_id")?,
        kind: row.get("kind")?,
        status: row.get("status")?,
        task: row.get("task")?,
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

#[cfg(test)]
mod commit_tests {
    use super::*;
    use crate::ingest::IngestBundle;
    use crate::migrations::tests::session;

    fn turn(id: &str, session_id: &str, seq: u32) -> Turn {
        Turn {
            id: id.into(),
            session_id: session_id.into(),
            seq,
            user_text: Some(format!("turn {id}")),
            assistant_text: None,
            started_at: None,
            ended_at: None,
            tokens: None,
            cost_usd: None,
        }
    }

    fn tool_call(id: &str, session_id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            session_id: session_id.into(),
            turn_seq: 1,
            name: "Read".into(),
            input: serde_json::json!({}),
            output: None,
            status: "ok".into(),
            duration_ms: None,
            created_at: None,
        }
    }

    fn bundle(sessions: Vec<Session>, turns: Vec<Turn>) -> IngestBundle {
        IngestBundle {
            sessions,
            turns,
            file_cursors: vec![(PathBuf::from("/logs/s1.jsonl"), 10, 20)],
            ..Default::default()
        }
    }

    #[test]
    fn failed_commit_leaves_cursor_unadvanced() {
        let store = InsightsStore::open_in_memory().unwrap();
        let b = bundle(vec![session("s1")], vec![turn("t1", "not-in-bundle", 1)]);
        assert!(store.commit_ingest("claude_code", &b).is_err());
        assert!(store.load_file_cursors("claude_code").unwrap().is_empty());
        assert!(store.get_session("s1").unwrap().is_none());
    }

    #[test]
    fn second_commit_does_not_duplicate_children() {
        let store = InsightsStore::open_in_memory().unwrap();
        let mut b = bundle(
            vec![session("s1")],
            vec![turn("a", "s1", 1), turn("b", "s1", 2)],
        );
        b.tool_calls = vec![tool_call("tc1", "s1")];
        b.file_events = vec![crate::model::FileEvent {
            id: "fe1".into(),
            session_id: "s1".into(),
            path: "/p".into(),
            kind: "edit".into(),
            at: Utc::now(),
        }];
        b.subagents = vec![crate::model::Subagent {
            id: "sub1".into(),
            session_id: "s1".into(),
            parent_tool_call_id: None,
            kind: "task".into(),
            status: "done".into(),
            task: None,
        }];
        let first = store.commit_ingest("claude_code", &b).unwrap();
        let second = store.commit_ingest("claude_code", &b).unwrap();
        assert_eq!(first, second);
        assert_eq!((first.sessions, first.turns, first.tool_calls), (1, 2, 1));
        assert_eq!((first.file_events, first.subagents), (1, 1));
        assert_eq!(store.get_turns("s1").unwrap().len(), 2);
        assert_eq!(store.get_tool_calls("s1").unwrap().len(), 1);
        assert_eq!(store.get_file_events("s1").unwrap().len(), 1);
        assert_eq!(store.get_subagents("s1").unwrap().len(), 1);
        let cursors = store.load_file_cursors("claude_code").unwrap();
        assert_eq!(cursors.get(Path::new("/logs/s1.jsonl")), Some(&(10, 20)));
    }

    #[test]
    fn rewrite_with_shifted_seq_succeeds() {
        let store = InsightsStore::open_in_memory().unwrap();
        let b = bundle(
            vec![session("s1")],
            vec![turn("a", "s1", 1), turn("b", "s1", 2), turn("c", "s1", 3)],
        );
        store.commit_ingest("claude_code", &b).unwrap();
        let b = bundle(
            vec![session("s1")],
            vec![turn("d", "s1", 1), turn("e", "s1", 2)],
        );
        let stats = store.commit_ingest("claude_code", &b).unwrap();
        assert_eq!(stats.turns, 2);
        let ids: Vec<String> = store
            .get_turns("s1")
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, ["d", "e"]);
    }
}
