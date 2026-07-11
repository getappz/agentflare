use rusqlite::{Connection, OptionalExtension, params};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Relation {
    pub id: i64,
    pub source_id: i64,
    pub target_id: i64,
    pub relation: String,
    pub judgment_status: String,
    pub reason: Option<String>,
    pub evidence: Option<String>,
    pub confidence: Option<f64>,
    pub marked_by_actor: Option<String>,
    pub marked_by_kind: Option<String>,
    pub marked_by_model: Option<String>,
    pub session_id: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[allow(clippy::too_many_arguments)]
pub fn create(
    conn: &Connection,
    source_id: i64,
    target_id: i64,
    relation: &str,
    session_id: Option<&str>,
    reason: Option<&str>,
    evidence: Option<&str>,
    confidence: Option<f64>,
) -> rusqlite::Result<i64> {
    let now = now_iso();
    // `last_insert_rowid()` only reflects a successful INSERT: when the
    // ON CONFLICT ... DO UPDATE arm fires instead, SQLite does not update
    // it, so it can return an unrelated id. RETURNING gives us the actual
    // row id regardless of which arm ran.
    conn.query_row(
        "INSERT INTO memory_relations
            (source_id, target_id, relation, judgment_status, reason, evidence, confidence, session_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, 'judged', ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT(source_id, target_id, relation) DO UPDATE SET
            judgment_status = 'judged',
            reason = COALESCE(?4, reason),
            evidence = COALESCE(?5, evidence),
            confidence = COALESCE(?6, confidence),
            updated_at = ?8
         RETURNING id",
        params![source_id, target_id, relation, reason, evidence, confidence, session_id, now],
        |r| r.get(0),
    )
}

#[allow(dead_code)]
pub fn get(conn: &Connection, id: i64) -> rusqlite::Result<Option<Relation>> {
    conn.query_row(
        "SELECT id, source_id, target_id, relation, judgment_status, reason, evidence,
                confidence, marked_by_actor, marked_by_kind, marked_by_model,
                session_id, created_at, updated_at
         FROM memory_relations WHERE id = ?1",
        params![id],
        map_relation,
    )
    .optional()
}

#[allow(dead_code)]
pub fn list_for_observation(conn: &Connection, obs_id: i64) -> rusqlite::Result<Vec<Relation>> {
    let mut stmt = conn.prepare(
        "SELECT id, source_id, target_id, relation, judgment_status, reason, evidence,
                confidence, marked_by_actor, marked_by_kind, marked_by_model,
                session_id, created_at, updated_at
         FROM memory_relations
         WHERE source_id = ?1 OR target_id = ?1
         ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map(params![obs_id], map_relation)?;
    rows.collect()
}

#[allow(dead_code)]
fn map_relation(r: &rusqlite::Row<'_>) -> rusqlite::Result<Relation> {
    Ok(Relation {
        id: r.get(0)?,
        source_id: r.get(1)?,
        target_id: r.get(2)?,
        relation: r.get(3)?,
        judgment_status: r.get(4)?,
        reason: r.get(5)?,
        evidence: r.get(6)?,
        confidence: r.get(7)?,
        marked_by_actor: r.get(8)?,
        marked_by_kind: r.get(9)?,
        marked_by_model: r.get(10)?,
        session_id: r.get(11)?,
        created_at: r.get(12)?,
        updated_at: r.get(13)?,
    })
}

fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{observations, schema};

    fn new_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::migrate(&conn).unwrap();
        conn
    }

    fn make_obs(conn: &Connection, title: &str) -> i64 {
        match observations::save(conn, None, "note", title, "content", None, None, None, None)
            .unwrap()
        {
            observations::SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        }
    }

    // Regression test: re-relating the same (source, target, relation)
    // triple used to be able to return an unrelated id, because
    // last_insert_rowid() doesn't reflect the ON CONFLICT DO UPDATE path.
    #[test]
    fn create_twice_with_same_triple_returns_same_id() {
        let conn = new_db();
        let source = make_obs(&conn, "source obs");
        let target = make_obs(&conn, "target obs");

        let id1 = create(
            &conn,
            source,
            target,
            "related",
            None,
            Some("first reason"),
            None,
            None,
        )
        .unwrap();
        let id2 = create(
            &conn,
            source,
            target,
            "related",
            None,
            Some("second reason"),
            None,
            None,
        )
        .unwrap();

        assert_eq!(id1, id2);
        let rel = get(&conn, id1).unwrap().unwrap();
        assert_eq!(rel.reason.as_deref(), Some("second reason"));
    }
}
