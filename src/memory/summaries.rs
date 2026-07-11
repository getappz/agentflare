use rusqlite::{params, Connection};

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSummary {
    pub id: i64,
    pub project: String,
    pub session_id: Option<String>,
    pub seq: i64,
    pub summary: String,
    pub searchable_text: String,
    pub created_at: String,
}

pub fn append(
    conn: &Connection,
    project: &str,
    session_id: Option<&str>,
    summary: &str,
) -> rusqlite::Result<i64> {
    let now = now_iso();
    let searchable = format!("{}: {}\n{}", project, summary, now);
    // `session_summaries` has UNIQUE(project, seq). A separate
    // SELECT MAX(seq)+1 followed by INSERT isn't atomic: two concurrent
    // callers can read the same MAX and then one INSERT fails the UNIQUE
    // constraint. A single INSERT...SELECT computes the seq inline as part
    // of the same statement, so the read and write are atomic under
    // SQLite's own per-statement locking — no explicit transaction needed
    // here (important: `append` is also called from inside
    // `mcp::handle_handoff`'s own transaction, where starting a nested
    // `BEGIN` would error).
    conn.execute(
        "INSERT INTO session_summaries (project, session_id, seq, summary, searchable_text, created_at)
         SELECT ?1, ?2, COALESCE(MAX(seq), 0) + 1, ?3, ?4, ?5
         FROM session_summaries WHERE project = ?1",
        params![project, session_id, summary, searchable, now],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn list_recent(conn: &Connection, project: Option<&str>, limit: usize) -> rusqlite::Result<Vec<SessionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT id, project, session_id, seq, summary, searchable_text, created_at
         FROM session_summaries
         WHERE (?1 IS NULL OR project = ?1)
         ORDER BY created_at DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![project, limit as i64], |r| {
        Ok(SessionSummary {
            id: r.get(0)?,
            project: r.get(1)?,
            session_id: r.get(2)?,
            seq: r.get(3)?,
            summary: r.get(4)?,
            searchable_text: r.get(5)?,
            created_at: r.get(6)?,
        })
    })?;
    rows.collect()
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
    fn append_assigns_increasing_per_project_seq() {
        let conn = new_db();
        let id1 = append(&conn, "proj-a", None, "first").unwrap();
        let id2 = append(&conn, "proj-a", None, "second").unwrap();
        let id3 = append(&conn, "proj-b", None, "other project").unwrap();
        assert_ne!(id1, id2);

        let recent = list_recent(&conn, Some("proj-a"), 10).unwrap();
        let seqs: Vec<i64> = recent.iter().map(|s| s.seq).collect();
        assert!(seqs.contains(&1) && seqs.contains(&2));

        // A different project's seq counter is independent, so proj-b's
        // first summary also starts at seq 1, not 3.
        let proj_b = list_recent(&conn, Some("proj-b"), 10).unwrap();
        assert_eq!(proj_b.len(), 1);
        assert_eq!(proj_b[0].seq, 1);
        let _ = id3;
    }

    // Regression guard: `append` must remain safe to call from inside a
    // caller-managed transaction (as mcp::handle_handoff does) — it must
    // not start its own nested transaction, which SQLite rejects.
    #[test]
    fn append_works_inside_an_existing_transaction() {
        let conn = new_db();
        let tx = conn.unchecked_transaction().unwrap();
        let id = append(&tx, "proj-a", None, "inside tx").unwrap();
        tx.commit().unwrap();
        assert!(id > 0);
        let recent = list_recent(&conn, Some("proj-a"), 10).unwrap();
        assert_eq!(recent.len(), 1);
    }
}
