use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Session {
    pub id: String,
    pub project: Option<String>,
    pub directory: Option<String>,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub summary: Option<String>,
    pub status: String,
    pub task: Option<String>,
    pub findings: String,
    pub decisions: String,
    pub files_touched: String,
    pub evidence: String,
    pub stats: String,
    pub compaction_snapshot: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

pub fn create(
    conn: &Connection,
    id: &str,
    project: Option<&str>,
    directory: Option<&str>,
) -> rusqlite::Result<Session> {
    let now = now_iso();
    conn.execute(
        "INSERT INTO sessions (id, project, directory, started_at, status, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, 'active', ?4, ?4)",
        params![id, project, directory, now],
    )?;
    get(conn, id)?.ok_or_else(|| rusqlite::Error::InvalidParameterName("insert succeeded but readback failed".into()))
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<Session>> {
    conn.query_row(
        "SELECT id, project, directory, started_at, ended_at, summary, status,
                task, findings, decisions, files_touched, evidence, stats,
                compaction_snapshot, created_at, updated_at
         FROM sessions WHERE id = ?1",
        params![id],
        |r| {
            Ok(Session {
                id: r.get(0)?,
                project: r.get(1)?,
                directory: r.get(2)?,
                started_at: r.get(3)?,
                ended_at: r.get(4)?,
                summary: r.get(5)?,
                status: r.get(6)?,
                task: r.get(7)?,
                findings: r.get(8)?,
                decisions: r.get(9)?,
                files_touched: r.get(10)?,
                evidence: r.get(11)?,
                stats: r.get(12)?,
                compaction_snapshot: r.get(13)?,
                created_at: r.get(14)?,
                updated_at: r.get(15)?,
            })
        },
    ).optional()
}

pub fn update_status(conn: &Connection, id: &str, status: &str) -> rusqlite::Result<()> {
    let now = now_iso();
    conn.execute(
        "UPDATE sessions SET status = ?2, updated_at = ?3 WHERE id = ?1",
        params![id, status, now],
    )?;
    Ok(())
}

pub fn close(conn: &Connection, id: &str, summary: &str) -> rusqlite::Result<()> {
    let now = now_iso();
    conn.execute(
        "UPDATE sessions SET status = 'closed', ended_at = ?2, summary = ?3, updated_at = ?2 WHERE id = ?1",
        params![id, now, summary],
    )?;
    Ok(())
}

pub fn update_enriched(
    conn: &Connection,
    id: &str,
    task: Option<&str>,
    findings: Option<&str>,
    decisions: Option<&str>,
    files_touched: Option<&str>,
    evidence: Option<&str>,
    stats: Option<&str>,
    compaction_snapshot: Option<&str>,
) -> rusqlite::Result<()> {
    let now = now_iso();
    conn.execute(
        "UPDATE sessions SET
            task = COALESCE(?2, task),
            findings = COALESCE(?3, findings),
            decisions = COALESCE(?4, decisions),
            files_touched = COALESCE(?5, files_touched),
            evidence = COALESCE(?6, evidence),
            stats = COALESCE(?7, stats),
            compaction_snapshot = COALESCE(?8, compaction_snapshot),
            updated_at = ?9
         WHERE id = ?1",
        params![id, task, findings, decisions, files_touched, evidence, stats, compaction_snapshot, now],
    )?;
    Ok(())
}

pub fn list_recent(conn: &Connection, project: Option<&str>, limit: usize) -> rusqlite::Result<Vec<Session>> {
    let mut stmt = conn.prepare(
        "SELECT id, project, directory, started_at, ended_at, summary, status,
                task, findings, decisions, files_touched, evidence, stats,
                compaction_snapshot, created_at, updated_at
         FROM sessions
         WHERE (?1 IS NULL OR project = ?1)
         ORDER BY started_at DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project, limit as i64], |r| {
        Ok(Session {
            id: r.get(0)?,
            project: r.get(1)?,
            directory: r.get(2)?,
            started_at: r.get(3)?,
            ended_at: r.get(4)?,
            summary: r.get(5)?,
            status: r.get(6)?,
            task: r.get(7)?,
            findings: r.get(8)?,
            decisions: r.get(9)?,
            files_touched: r.get(10)?,
            evidence: r.get(11)?,
            stats: r.get(12)?,
            compaction_snapshot: r.get(13)?,
            created_at: r.get(14)?,
            updated_at: r.get(15)?,
        })
    })?;
    rows.collect()
}

pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    let n = conn.execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
    Ok(n > 0)
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::schema;

    fn new_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn create_get_close_roundtrip() {
        let conn = new_db();
        let created = create(&conn, "sess-1", Some("proj-a"), Some("/repo")).unwrap();
        assert_eq!(created.status, "active");
        assert_eq!(created.project.as_deref(), Some("proj-a"));

        let fetched = get(&conn, "sess-1").unwrap().unwrap();
        assert_eq!(fetched.id, "sess-1");

        close(&conn, "sess-1", "wrapped up").unwrap();
        let closed = get(&conn, "sess-1").unwrap().unwrap();
        assert_eq!(closed.status, "closed");
        assert_eq!(closed.summary.as_deref(), Some("wrapped up"));
        assert!(closed.ended_at.is_some());
    }

    #[test]
    fn get_missing_session_returns_none() {
        let conn = new_db();
        assert!(get(&conn, "does-not-exist").unwrap().is_none());
    }
}
