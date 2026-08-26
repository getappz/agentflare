//! Vector storage for semantic skill search. Mirrors `src/memory/embeddings.rs`'s
//! `observations_vec` pattern, keyed by (name, source) instead of a single id.
//! Pure storage/lookup -- callers supply the actual embedding vectors (see
//! `search::search_semantic`), so this module has no opinion on which model
//! or engine produced them.

use agentflare_store::embed::{bytes_to_vec, cosine_similarity, vec_to_bytes};
use rusqlite::{Connection, params};

// Unix epoch seconds as text -- this crate doesn't otherwise depend on
// chrono, and pulling it in for one timestamp column isn't worth it.
fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    secs.to_string()
}

pub fn upsert(
    conn: &Connection,
    name: &str,
    source: &str,
    vec: &[f32],
    model: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO skills_vec (name, source, embedding, dim, model, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(name, source) DO UPDATE SET
           embedding = excluded.embedding, dim = excluded.dim,
           model = excluded.model, updated_at = excluded.updated_at",
        params![
            name,
            source,
            vec_to_bytes(vec),
            vec.len() as i64,
            model,
            now_iso()
        ],
    )?;
    Ok(())
}

/// Drop a skill's vector. Used when reindexing can't produce a fresh one, so
/// a stale embedding never outlives the content it described -- `missing`
/// then re-surfaces the row for the next backfill. No-op if none exists.
pub fn delete(conn: &Connection, name: &str, source: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM skills_vec WHERE name = ?1 AND source = ?2",
        params![name, source],
    )?;
    Ok(())
}

/// Indexed skills that have no embedding yet, as (name, source, "description\ntags").
pub fn missing(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<(String, String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT s.name, s.source, s.description, s.tags
         FROM skills s
         LEFT JOIN skills_vec v ON v.name = s.name AND v.source = s.source
         WHERE v.name IS NULL
         ORDER BY s.name, s.source LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit as i64], |r| {
        let name: String = r.get(0)?;
        let source: String = r.get(1)?;
        let description: String = r.get(2)?;
        let tags: String = r.get(3)?;
        Ok((name, source, format!("{description}\n{tags}")))
    })?;
    rows.collect()
}

/// Compute and store embeddings for up to `limit` skills that don't have one
/// yet, using `embed_doc` (caller-supplied -- this crate has no engine of
/// its own). Returns how many were actually embedded; a skill whose
/// `embed_doc` call returns `None` (feature off, or a failed call) is left
/// for the next backfill rather than recorded as an empty vector.
pub fn backfill(
    conn: &Connection,
    embed_doc: impl Fn(&str) -> Option<Vec<f32>>,
    model: &str,
    limit: usize,
) -> rusqlite::Result<usize> {
    let mut done = 0;
    for (name, source, text) in missing(conn, limit)? {
        if let Some(vec) = embed_doc(&text) {
            upsert(conn, &name, &source, &vec, model)?;
            done += 1;
        }
    }
    Ok(done)
}

// flare-code: brute-force scan; move to ANN/HNSW only if skill count makes
// this measurably slow (matches the identical tradeoff in memory::embeddings).
pub fn candidates(
    conn: &Connection,
    query_vec: &[f32],
    k: usize,
) -> rusqlite::Result<Vec<((String, String), f64)>> {
    let mut stmt = conn.prepare("SELECT name, source, embedding FROM skills_vec")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Vec<u8>>(2)?,
        ))
    })?;
    let mut scored: Vec<((String, String), f64)> = rows
        .filter_map(|row| {
            let (name, source, blob) = row.ok()?;
            let emb = bytes_to_vec(&blob)?;
            let sim = cosine_similarity(query_vec, &emb)? as f64;
            Some(((name, source), sim))
        })
        .collect();
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(k);
    Ok(scored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn new_db() -> Connection {
        db::open_in_memory().unwrap()
    }

    fn insert_skill(conn: &Connection, name: &str, source: &str, description: &str) {
        conn.execute(
            "INSERT INTO skills (name, source, path, description) VALUES (?1, ?2, '/p', ?3)",
            params![name, source, description],
        )
        .unwrap();
    }

    #[test]
    fn upsert_roundtrip_and_missing_shrinks() {
        let conn = new_db();
        insert_skill(&conn, "alpha", "local", "alpha desc");
        insert_skill(&conn, "beta", "local", "beta desc");
        assert_eq!(missing(&conn, 10).unwrap().len(), 2);
        upsert(&conn, "alpha", "local", &[1.0, 0.0], "test-model").unwrap();
        let left = missing(&conn, 10).unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!((left[0].0.as_str(), left[0].1.as_str()), ("beta", "local"));
        // re-upsert replaces, not duplicates
        upsert(&conn, "alpha", "local", &[0.0, 1.0], "test-model").unwrap();
        assert_eq!(missing(&conn, 10).unwrap().len(), 1);
    }

    #[test]
    fn candidates_rank_by_cosine() {
        let conn = new_db();
        insert_skill(&conn, "aligned", "local", "d");
        insert_skill(&conn, "orthogonal", "local", "d");
        upsert(&conn, "aligned", "local", &[1.0, 0.0], "m").unwrap();
        upsert(&conn, "orthogonal", "local", &[0.0, 1.0], "m").unwrap();
        let hits = candidates(&conn, &[1.0, 0.0], 10).unwrap();
        assert_eq!(hits[0].0, ("aligned".to_string(), "local".to_string()));
        assert!(hits[0].1 > 0.99);
    }

    #[test]
    fn delete_drops_vector_and_resurfaces_in_missing() {
        let conn = new_db();
        insert_skill(&conn, "stale", "local", "d");
        upsert(&conn, "stale", "local", &[1.0, 0.0], "m").unwrap();
        assert!(missing(&conn, 10).unwrap().is_empty());
        delete(&conn, "stale", "local").unwrap();
        assert!(candidates(&conn, &[1.0, 0.0], 10).unwrap().is_empty());
        assert_eq!(missing(&conn, 10).unwrap().len(), 1);
        // deleting a row with no vector is a harmless no-op
        delete(&conn, "stale", "local").unwrap();
    }

    #[test]
    fn deleting_the_skill_cascades_its_vector() {
        let conn = new_db();
        insert_skill(&conn, "gone", "local", "d");
        upsert(&conn, "gone", "local", &[1.0, 0.0], "m").unwrap();
        db::delete_skill(&conn, "gone", "local").unwrap();
        assert!(candidates(&conn, &[1.0, 0.0], 10).unwrap().is_empty());
    }

    #[test]
    fn backfill_embeds_missing_and_skips_failed_calls() {
        let conn = new_db();
        insert_skill(&conn, "embeddable", "local", "d");
        insert_skill(&conn, "unembeddable", "local", "d");
        let done = backfill(
            &conn,
            |text| {
                if text.starts_with('d') {
                    Some(vec![1.0, 0.0])
                } else {
                    None
                }
            },
            "test-model",
            10,
        )
        .unwrap();
        // both texts are "d\n" (description "d", empty tags) -- both embed.
        assert_eq!(done, 2);
        assert!(missing(&conn, 10).unwrap().is_empty());
    }

    #[test]
    fn backfill_leaves_failed_embeds_for_next_pass() {
        let conn = new_db();
        insert_skill(&conn, "only", "local", "d");
        let done = backfill(&conn, |_| None, "test-model", 10).unwrap();
        assert_eq!(done, 0);
        assert_eq!(missing(&conn, 10).unwrap().len(), 1);
    }
}
