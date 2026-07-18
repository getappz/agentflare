use rusqlite::{Connection, params};

use super::observations::Observation;

pub fn search(
    conn: &Connection,
    query: &str,
    project: Option<&str>,
    r#type: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<Observation>> {
    if query.trim().is_empty() {
        return search_fallback(conn, project, r#type, limit);
    }
    let limit = limit.min(50) as i64;
    let like_pat = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));

    let results: Vec<Observation> = fts_ranked(conn, query, project, r#type, limit as usize)?
        .into_iter()
        .map(|(o, _)| o)
        .collect();
    if !results.is_empty() {
        return Ok(results);
    }

    let like_sql = "SELECT o.id, o.session_id, o.type, o.title, o.content, o.tool_name,
                o.project, o.scope, o.topic_key, o.normalized_hash,
                o.revision_count, o.duplicate_count, o.last_seen_at,
                o.review_after, o.pinned, o.created_at, o.updated_at, o.deleted_at
         FROM observations o
         WHERE o.deleted_at IS NULL
           AND (o.title LIKE ?1 ESCAPE '\\' OR o.content LIKE ?1 ESCAPE '\\')
           AND (?3 IS NULL OR o.project = ?3)
           AND (?4 IS NULL OR o.type = ?4)
         ORDER BY o.created_at DESC
         LIMIT ?2";
    let mut like_stmt = conn.prepare(like_sql)?;
    let like_rows =
        like_stmt.query_map(params![like_pat, limit, project, r#type], map_search_row)?;
    like_rows.collect()
}

fn search_fallback(
    conn: &Connection,
    project: Option<&str>,
    r#type: Option<&str>,
    limit: usize,
) -> rusqlite::Result<Vec<Observation>> {
    super::observations::list_recent(conn, project, r#type, limit)
}

fn build_fts_query(raw: &str) -> String {
    let tokens: Vec<String> = raw
        .split_whitespace()
        .map(|t| t.replace('"', "")) // strip embedded quotes, then quote whole token
        .filter(|t| !t.is_empty())
        .map(|t| format!("\"{t}\""))
        .collect();
    if tokens.is_empty() {
        // Every token sanitized to empty (e.g. raw == "***"). Fall back to a
        // safely quoted/escaped version of the raw query instead of passing
        // it through verbatim, which would re-introduce FTS5 syntax-error risk.
        return format!("\"{}\"", raw.replace('"', "\"\""));
    }
    tokens.join(" ")
}

/// FTS hits with HIGHER-IS-BETTER scores (negated SQLite bm25 rank).
///
/// Filters are always present as `(?N IS NULL OR ...)` clauses with their
/// own distinct placeholder index, rather than conditionally spliced-in
/// SQL fragments sharing an index with LIMIT — the latter is what caused
/// the ?2 collision bug (project filter and LIMIT both bound to ?2).
fn fts_ranked(
    conn: &Connection,
    query: &str,
    project: Option<&str>,
    r#type: Option<&str>,
    k: usize,
) -> rusqlite::Result<Vec<(Observation, f64)>> {
    let sql = "SELECT o.id, o.session_id, o.type, o.title, o.content, o.tool_name,
                o.project, o.scope, o.topic_key, o.normalized_hash,
                o.revision_count, o.duplicate_count, o.last_seen_at,
                o.review_after, o.pinned, o.created_at, o.updated_at, o.deleted_at,
                -bm25(observations_fts, 3.0, 1.0, 1.0, 1.0, 1.0) AS score
         FROM observations_fts f
         JOIN observations o ON o.id = f.rowid
         WHERE observations_fts MATCH ?1
           AND o.deleted_at IS NULL
           AND (?3 IS NULL OR o.project = ?3)
           AND (?4 IS NULL OR o.type = ?4)
         ORDER BY bm25(observations_fts, 3.0, 1.0, 1.0, 1.0, 1.0)
         LIMIT ?2";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(
        params![build_fts_query(query), k as i64, project, r#type],
        |r| Ok((map_search_row(r)?, r.get::<_, f64>(18)?)),
    )?;
    rows.collect()
}

/// Hybrid recall: weighted BM25+vector merge (0.4/0.6), 30-day temporal
/// decay, MMR re-rank (λ=0.7). `embed` returning `None` (no `semantic`
/// feature, no model) makes this EXACTLY equivalent to `search()` — the
/// equality test below enforces that contract.
pub fn search_hybrid(
    conn: &Connection,
    query: &str,
    project: Option<&str>,
    r#type: Option<&str>,
    limit: usize,
    embed: impl Fn(&str) -> Option<Vec<f32>>,
) -> rusqlite::Result<Vec<Observation>> {
    use agentflare_store::retrieval;

    let Some(qvec) = embed(query) else {
        return search(conn, query, project, r#type, limit);
    };
    let pool = limit.min(50) * 2;

    let fts = fts_ranked(conn, query, project, r#type, pool)?;
    let vec_hits = super::embeddings::candidates(conn, &qvec, project, r#type, pool)?;
    if fts.is_empty() && vec_hits.is_empty() {
        // Preserve the LIKE/recent fallback behavior for no-hit queries.
        return search(conn, query, project, r#type, limit);
    }

    let bm25_pairs: Vec<(i64, f64)> = fts.iter().map(|(o, s)| (o.id, *s)).collect();
    let mut merged = retrieval::merge_ranked(&bm25_pairs, &vec_hits, 0.4, 0.6);

    // Materialize observations for ids that only came from the vector side.
    let mut by_id: std::collections::HashMap<i64, Observation> =
        fts.into_iter().map(|(o, _)| (o.id, o)).collect();
    for (id, _) in &merged {
        if !by_id.contains_key(id) {
            if let Some(o) = super::observations::get(conn, *id)? {
                by_id.insert(*id, o);
            }
        }
    }
    merged.retain(|(id, _)| by_id.contains_key(id));

    // Temporal decay: 30-day half-life on RFC3339 created_at.
    let now = chrono::Utc::now();
    for (id, score) in merged.iter_mut() {
        if let Ok(created) = chrono::DateTime::parse_from_rfc3339(&by_id[id].created_at) {
            let age_days = (now - created.with_timezone(&chrono::Utc)).num_hours() as f64 / 24.0;
            *score *= retrieval::decay_factor(age_days.max(0.0), 30.0);
        }
    }

    // MMR: λ=0.7, jaccard similarity over title+content.
    merged.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let texts: Vec<String> = merged
        .iter()
        .map(|(id, _)| format!("{}\n{}", by_id[id].title, by_id[id].content))
        .collect();
    let scores: Vec<f64> = merged.iter().map(|(_, s)| *s).collect();
    let picked = retrieval::mmr_select(&scores, limit, 0.7, |i, j| {
        retrieval::jaccard_overlap(&texts[i], &texts[j])
    });

    Ok(picked
        .into_iter()
        .map(|i| by_id[&merged[i].0].clone())
        .collect())
}

fn map_search_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Observation> {
    Ok(Observation {
        id: r.get(0)?,
        session_id: r.get(1)?,
        r#type: r.get(2)?,
        title: r.get(3)?,
        content: r.get(4)?,
        tool_name: r.get(5)?,
        project: r.get(6)?,
        scope: r.get(7)?,
        topic_key: r.get(8)?,
        normalized_hash: r.get(9)?,
        revision_count: r.get(10)?,
        duplicate_count: r.get(11)?,
        last_seen_at: r.get(12)?,
        review_after: r.get(13)?,
        pinned: r.get(14)?,
        created_at: r.get(15)?,
        updated_at: r.get(16)?,
        deleted_at: r.get(17)?,
    })
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

    // Deterministic fake embedder: axis per keyword, no ONNX involved.
    fn fake_embed(text: &str) -> Option<Vec<f32>> {
        let t = text.to_lowercase();
        Some(vec![
            if t.contains("rocket") { 1.0 } else { 0.0 },
            if t.contains("garden") { 1.0 } else { 0.0 },
        ])
    }

    // The no-embedder contract: search_hybrid with a None embedder must be
    // EXACTLY today's FTS search — same rows, same order.
    #[test]
    fn hybrid_none_embedder_matches_plain_search_exactly() {
        let conn = new_db();
        for (title, content) in [("widget rollout", "shipping"), ("widget outage", "boom")] {
            observations::save(
                &conn, None, "note", title, content, None, Some("p"), None, None,
            )
            .unwrap();
        }
        let plain = search(&conn, "widget", Some("p"), None, 10).unwrap();
        let hybrid = search_hybrid(&conn, "widget", Some("p"), None, 10, |_| None).unwrap();
        assert_eq!(
            plain.iter().map(|o| o.id).collect::<Vec<_>>(),
            hybrid.iter().map(|o| o.id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn hybrid_surfaces_semantic_match_without_keyword() {
        let conn = new_db();
        // Content shares NO keyword with the query "rocket".
        let id = match observations::save(
            &conn,
            None,
            "note",
            "launch vehicle notes",
            "propulsion staging details",
            None,
            Some("p"),
            None,
            None,
        )
        .unwrap()
        {
            observations::SaveOutcome::Created(id) => id,
            other => panic!("{other:?}"),
        };
        crate::memory::embeddings::upsert(&conn, id, &[1.0, 0.0], "fake").unwrap();
        let hits = search_hybrid(&conn, "rocket", Some("p"), None, 10, fake_embed).unwrap();
        assert!(
            hits.iter().any(|o| o.id == id),
            "vector-only match must surface"
        );
    }

    #[test]
    fn hybrid_decay_downranks_old_equal_match() {
        let conn = new_db();
        let mut ids = Vec::new();
        for title in ["garden plan a", "garden plan b"] {
            if let observations::SaveOutcome::Created(id) = observations::save(
                &conn,
                None,
                "note",
                title,
                &format!("unique-filler-{title}"),
                None,
                Some("p"),
                None,
                None,
            )
            .unwrap()
            {
                crate::memory::embeddings::upsert(&conn, id, &[0.0, 1.0], "fake").unwrap();
                ids.push(id);
            }
        }
        // Age the first one by a year.
        conn.execute(
            "UPDATE observations SET created_at = '2025-07-19T00:00:00+00:00' WHERE id = ?1",
            rusqlite::params![ids[0]],
        )
        .unwrap();
        let hits = search_hybrid(&conn, "garden", Some("p"), None, 2, fake_embed).unwrap();
        assert_eq!(
            hits[0].id, ids[1],
            "fresh observation must outrank year-old twin"
        );
    }

    // Regression test for the ?2/LIMIT collision: project-scoped search used
    // to error with InvalidParameterCount (3 values bound against 2 unique
    // placeholder indices) on every call. Also proves cross-project isolation.
    #[test]
    fn project_scoped_search_returns_only_matching_project() {
        let conn = new_db();
        observations::save(
            &conn,
            None,
            "note",
            "widget rollout",
            "shipping the widget rollout",
            None,
            Some("proj-a"),
            None,
            None,
        )
        .unwrap();
        observations::save(
            &conn,
            None,
            "note",
            "widget outage",
            "widget rollout caused an outage",
            None,
            Some("proj-b"),
            None,
            None,
        )
        .unwrap();

        let results = search(&conn, "widget", Some("proj-a"), None, 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].project.as_deref(), Some("proj-a"));

        // Exercise the LIKE fallback path too: a query that FTS5 tokenizes
        // to nothing meaningful for one side still respects project scope.
        let results_b = search(&conn, "outage", Some("proj-b"), None, 10).unwrap();
        assert_eq!(results_b.len(), 1);
        assert_eq!(results_b[0].project.as_deref(), Some("proj-b"));
    }

    #[test]
    fn build_fts_query_sanitizes_all_punctuation_input() {
        // "***" sanitizes to no tokens; must not be returned verbatim.
        let q = build_fts_query("***");
        assert_eq!(q, "\"***\"");
    }

    #[test]
    fn type_filter_applied_in_sql_not_post_fetch() {
        let conn = new_db();
        // All 6 rows share the token "marker" so the FTS query matches all
        // of them; only the decision row has the target type. With more
        // FTS-matching non-target rows than `limit`, a post-fetch filter
        // (fetch top-`limit` by rank, then filter by type) could drop the
        // matching row entirely depending on rank order — the filter must
        // be applied in SQL before LIMIT to reliably surface it.
        for i in 0..5 {
            observations::save(
                &conn,
                None,
                "bugfix",
                &format!("bug {i}"),
                "marker filler content",
                None,
                Some("proj-a"),
                None,
                None,
            )
            .unwrap();
        }
        observations::save(
            &conn,
            None,
            "decision",
            "the decision",
            "marker decided content",
            None,
            Some("proj-a"),
            None,
            None,
        )
        .unwrap();

        let results = search(&conn, "marker", Some("proj-a"), Some("decision"), 3).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].r#type, "decision");
    }

    // Regression test for the weighted bm25() ranking, mirroring
    // skill-registry's `name_match_outranks_description_match`: a query term
    // that's literally the observation's title should outrank an
    // observation where the term only appears buried in the content.
    #[test]
    fn title_match_outranks_content_match() {
        let conn = new_db();
        observations::save(
            &conn,
            None,
            "note",
            "zephyr configuration guide",
            "some filler text about unrelated setup steps and general notes",
            None,
            Some("proj-a"),
            None,
            None,
        )
        .unwrap();
        observations::save(
            &conn,
            None,
            "note",
            "unrelated title about widgets",
            "this note mentions zephyr only once buried among filler words describing other things",
            None,
            Some("proj-a"),
            None,
            None,
        )
        .unwrap();

        let results = search(&conn, "zephyr", Some("proj-a"), None, 10).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "zephyr configuration guide");
    }
}
