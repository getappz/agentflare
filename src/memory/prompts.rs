use rusqlite::{params, Connection};

pub fn save(conn: &Connection, session_id: Option<&str>, content: &str, project: Option<&str>) -> rusqlite::Result<i64> {
    let now = now_iso();
    conn.execute(
        "INSERT INTO user_prompts (session_id, content, project, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![session_id, content, project, now],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_recent(conn: &Connection, project: Option<&str>, limit: usize) -> rusqlite::Result<Vec<(i64, String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT id, content, project, created_at FROM user_prompts
         WHERE (?1 IS NULL OR project = ?1)
         ORDER BY created_at DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project, limit as i64], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    rows.collect()
}

fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}
