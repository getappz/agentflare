use agentflare_store::embed::{bytes_to_vec, cosine_similarity, vec_to_bytes};
use rusqlite::{Connection, params};

fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub fn upsert(conn: &Connection, obs_id: i64, vec: &[f32], model: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO observations_vec (obs_id, embedding, dim, model, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(obs_id) DO UPDATE SET
           embedding = excluded.embedding, dim = excluded.dim,
           model = excluded.model, updated_at = excluded.updated_at",
        params![obs_id, vec_to_bytes(vec), vec.len() as i64, model, now_iso()],
    )?;
    Ok(())
}

/// Live observations that have no embedding yet, as (id, "title\ncontent").
pub fn missing(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT o.id, o.title || char(10) || o.content
         FROM observations o
         LEFT JOIN observations_vec v ON v.obs_id = o.id
         WHERE v.obs_id IS NULL AND o.deleted_at IS NULL
         ORDER BY o.id LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
}

// flare-code: brute-force scan; move to ANN/HNSW only if observation count
// makes this measurably slow (openpawz runs the same strategy at <100K rows).
pub fn candidates(
    conn: &Connection,
    query_vec: &[f32],
    project: Option<&str>,
    r#type: Option<&str>,
    k: usize,
) -> rusqlite::Result<Vec<(i64, f64)>> {
    let mut stmt = conn.prepare(
        "SELECT o.id, v.embedding
         FROM observations_vec v
         JOIN observations o ON o.id = v.obs_id
         WHERE o.deleted_at IS NULL
           AND (?1 IS NULL OR o.project = ?1)
           AND (?2 IS NULL OR o.type = ?2)",
    )?;
    let rows = stmt.query_map(params![project, r#type], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    let mut scored: Vec<(i64, f64)> = rows
        .filter_map(|row| {
            let (id, blob) = row.ok()?;
            let emb = bytes_to_vec(&blob)?;
            let sim = cosine_similarity(query_vec, &emb)? as f64;
            Some((id, sim))
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    Ok(scored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{observations, schema};
    use rusqlite::Connection;

    fn new_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        schema::migrate(&mut conn).unwrap();
        conn
    }

    fn save_note(conn: &Connection, title: &str, project: &str) -> i64 {
        match observations::save(
            conn,
            None,
            "note",
            title,
            "content",
            None,
            Some(project),
            None,
            None,
        )
        .unwrap()
        {
            observations::SaveOutcome::Created(id) => id,
            other => panic!("expected Created, got {other:?}"),
        }
    }

    #[test]
    fn upsert_roundtrip_and_missing_shrinks() {
        let conn = new_db();
        let a = save_note(&conn, "alpha", "p");
        let b = save_note(&conn, "beta", "p");
        assert_eq!(missing(&conn, 10).unwrap().len(), 2);
        upsert(&conn, a, &[1.0, 0.0], "test-model").unwrap();
        let left = missing(&conn, 10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].0, b);
        // re-upsert replaces, not duplicates
        upsert(&conn, a, &[0.0, 1.0], "test-model").unwrap();
        assert_eq!(missing(&conn, 10).unwrap().len(), 1);
    }

    #[test]
    fn candidates_rank_by_cosine_and_respect_filters() {
        let conn = new_db();
        let a = save_note(&conn, "aligned", "p");
        let b = save_note(&conn, "orthogonal", "p");
        let c = save_note(&conn, "other-project", "q");
        upsert(&conn, a, &[1.0, 0.0], "m").unwrap();
        upsert(&conn, b, &[0.0, 1.0], "m").unwrap();
        upsert(&conn, c, &[1.0, 0.0], "m").unwrap();
        let hits = candidates(&conn, &[1.0, 0.0], Some("p"), None, 10).unwrap();
        assert_eq!(hits[0].0, a);
        assert!(hits[0].1 > 0.99);
        assert!(!hits.iter().any(|(id, _)| *id == c), "project filter leaked");
    }

    #[test]
    fn soft_deleted_observations_never_surface() {
        let conn = new_db();
        let a = save_note(&conn, "gone", "p");
        upsert(&conn, a, &[1.0, 0.0], "m").unwrap();
        observations::soft_delete(&conn, a).unwrap();
        assert!(
            candidates(&conn, &[1.0, 0.0], None, None, 10)
                .unwrap()
                .is_empty()
        );
        assert!(missing(&conn, 10).unwrap().is_empty());
    }
}
