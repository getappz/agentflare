use crate::Store;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::time::Instant;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub content: String,
    pub title: String,
    pub doc_type: String,
    pub blob_hash: Option<String>,
    pub mime: String,
    pub tags: Vec<String>,
    pub session_id: Option<String>,
    pub source: String,
    pub version: i32,
    pub created_at: i64,
    pub updated_at: i64,
    pub metadata: String,
    pub size: i64,
    pub deleted_at: Option<i64>,
}

/// A document without its body.
///
/// Exists because enumerating a store and reading a store are different
/// questions, and only one of them needs the text. A caching store can hold
/// tens of thousands of documents whose bodies run to megabytes in total;
/// serializing all of that to answer "what is in here?" is pure waste, and
/// for an MCP caller it is worse than waste — the response does not fit.
/// `content_bytes` is kept so a caller can still distinguish an empty
/// placeholder from a real page without being handed the page.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSummary {
    pub id: String,
    pub path: String,
    pub title: String,
    pub doc_type: String,
    pub source: String,
    pub version: i32,
    pub created_at: i64,
    pub updated_at: i64,
    pub content_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocVersion {
    pub id: String,
    pub doc_id: String,
    pub version: i32,
    pub content: String,
    pub blob_hash: Option<String>,
    pub mime: String,
    pub title: String,
    pub metadata: String,
    pub size: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocMatch {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub snippet: String,
    pub score: f64,
}

#[derive(Debug)]
pub struct DocUpsertOpts {
    pub title: Option<String>,
    pub doc_type: Option<String>,
    pub blob_hash: Option<String>,
    pub mime: Option<String>,
    pub tags: Option<Vec<String>>,
    pub session_id: Option<String>,
    pub source: Option<String>,
    pub metadata: Option<String>,
    pub size: Option<i64>,
    /// Whether an update should snapshot the previous row into
    /// `store_doc_history`. Defaults to `true` to preserve existing
    /// behavior; cache-type consumers (e.g. flare-docs) that never need
    /// "what changed between refreshes" can set this to `false` to avoid
    /// unbounded history-table growth from repeated re-fetches.
    pub track_history: bool,
}

impl Default for DocUpsertOpts {
    fn default() -> Self {
        Self {
            title: None,
            doc_type: None,
            blob_hash: None,
            mime: None,
            tags: None,
            session_id: None,
            source: None,
            metadata: None,
            size: None,
            track_history: true,
        }
    }
}

impl Store {
    /// Repopulates the document search index from `store_documents`.
    ///
    /// `store_docs_fts` is an external-content fts5 table kept in sync by
    /// triggers (see `migrations::EXTERNAL_CONTENT_FTS_MIGRATION`), so this
    /// is only needed after something rewrites the base table behind the
    /// triggers' back -- notably `VACUUM`, which may renumber the implicit
    /// rowids the index is keyed on.
    ///
    /// Transactional: the clear and the refill are two statements, and
    /// stopping between them would leave the index empty rather than stale.
    pub fn doc_fts_rebuild(&self) -> rusqlite::Result<()> {
        let conn = self.conn();
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        tx.execute_batch(crate::migrations::FTS_REBUILD_SQL)?;
        tx.commit()
    }

    /// Sync `text-splitter` chunks for a document. Called after every
    /// `doc_upsert_with_opts` commit; idempotent (deletes then re-inserts).
    /// Uses `chunk::chunk_markdown` so heading-aware boundaries match AI
    /// Search's chunker. Triggers on `store_doc_chunks` keep
    /// `store_chunks_fts` in sync automatically.
    pub fn sync_chunks(&self, doc_id: &str, content: &str) -> rusqlite::Result<()> {
        let conn = self.conn();
        let tx = rusqlite::Transaction::new_unchecked(
            &conn,
            rusqlite::TransactionBehavior::Immediate,
        )?;
        // Remove old chunk vectors first (before chunks, or subselect is empty).
        tx.execute(
            "DELETE FROM store_chunk_vec WHERE chunk_id IN (SELECT id FROM store_doc_chunks WHERE doc_id = ?1)",
            params![doc_id],
        )?;
        #[cfg(feature = "vector")]
        let _ = tx.execute(
            "DELETE FROM store_chunk_vec0 WHERE rowid IN (SELECT rowid FROM store_doc_chunks WHERE doc_id = ?1)",
            params![doc_id],
        );
        tx.execute("DELETE FROM store_doc_chunks WHERE doc_id = ?1", params![doc_id])?;
        // Empty docs produce no chunks — still correct to have none.
        if !content.trim().is_empty() {
            let chunks = crate::chunk::chunk_markdown(content);
            let now = db_kit::ids::now();
            let mut chunk_ids = Vec::with_capacity(chunks.len());
            for (idx, chunk) in chunks.iter().enumerate() {
                let cid = crate::chunk::chunk_id(doc_id, idx);
                let tok = chunk.len() as i64; // char-count proxy; token-count via tiktoken optional future
                tx.execute(
                    "INSERT INTO store_doc_chunks (id, doc_id, chunk_index, content, token_count, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![cid, doc_id, idx as i64, chunk, tok, now],
                )?;
                chunk_ids.push(cid);
            }
            tx.commit()?;
            drop(conn);
            // Best-effort embeddings: if model available (fastembed cache hit), fill vectors.
            // Failure (no model / offline) keeps BM25-only search working.
            #[cfg(feature = "embeddings")]
            {
                if let Some(vecs) = crate::fastembed::try_embed_batch(&chunks) {
                    for (cid, vec) in chunk_ids.into_iter().zip(vecs) {
                        let _ = self.chunk_set_embedding(&cid, &vec, "bge-small-en-v1.5");
                    }
                }
            }
            return Ok(());
        }
        tx.commit()
    }

    /// Backfill chunks for existing docs that have no chunks yet (migration helper).
    /// Returns number of docs backfilled. Call once after enabling chunking on an
    /// existing DB; new upserts already call `sync_chunks` automatically.
    pub fn backfill_chunks(&self, limit: usize) -> rusqlite::Result<usize> {
        let ids: Vec<(String, String)> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(
                "SELECT d.id, d.content FROM store_documents d
                 LEFT JOIN store_doc_chunks c ON c.doc_id = d.id
                 WHERE d.deleted_at IS NULL AND c.doc_id IS NULL
                 LIMIT ?1",
            )?;
            let rows = stmt.query_map(params![limit as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let n = ids.len();
        for (doc_id, content) in ids {
            let _ = self.sync_chunks(&doc_id, &content);
        }
        Ok(n)
    }

    pub fn doc_chunks(&self, doc_id: &str) -> rusqlite::Result<Vec<(String, String, i64)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, content, chunk_index FROM store_doc_chunks WHERE doc_id = ?1 ORDER BY chunk_index",
        )?;
        let rows = stmt.query_map(params![doc_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        rows.collect()
    }

    /// Total chunk count (optionally per project) — used for scale warning.
    pub fn chunk_count(&self, project_id: Option<&str>) -> rusqlite::Result<usize> {
        let conn = self.conn();
        let n: i64 = if let Some(pid) = project_id {
            conn.query_row(
                "SELECT COUNT(*) FROM store_doc_chunks c JOIN store_documents d ON d.id = c.doc_id WHERE d.project_id = ?1 AND d.deleted_at IS NULL",
                params![pid],
                |r| r.get(0),
            )?
        } else {
            conn.query_row("SELECT COUNT(*) FROM store_doc_chunks", [], |r| r.get(0))?
        };
        Ok(n as usize)
    }

    /// Pure predicate for scale warning — testable without DB.
    pub fn should_warn(count: usize, elapsed_ms: u128) -> bool {
        count > 50_000 && elapsed_ms > 100
    }

    /// Scale warning: >50k chunks + >100ms query → suggest sqlite-vec ANN.
    /// Returns warning string if triggered, else None. Logs to stderr + tracing.
    pub fn scale_warning(&self, project_id: &str, elapsed_ms: u128) -> Option<String> {
        let count = self.chunk_count(Some(project_id)).ok()?;
        if Self::should_warn(count, elapsed_ms) {
            let msg = format!(
                "scale warning: {} chunks in project '{}', query took {}ms (>100ms threshold) — consider enabling sqlite-vec vec0 ANN (cargo feature `vector`) for sub-10ms vector search",
                count, project_id, elapsed_ms
            );
            eprintln!("⚠️  {}", msg);
            return Some(msg);
        }
        // Also check global count for cross-project total
        let global = self.chunk_count(None).ok()?;
        if Self::should_warn(global, elapsed_ms) {
            let msg = format!(
                "scale warning: {} total chunks, query took {}ms (>100ms) — enable sqlite-vec ANN",
                global, elapsed_ms
            );
            eprintln!("⚠️  {}", msg);
            return Some(msg);
        }
        None
    }

    /// Chunk-level BM25 search — returns `DocMatch` per chunk but deduplicated
    /// to best chunk per doc (so callers still get doc-level ranking).
    pub fn chunk_search(
        &self,
        project_id: &str,
        query: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<DocMatch>> {
        let start = Instant::now();
        let out = {
            let conn = self.conn();
            let fts_query = flare_search_kit::fts_phrase_query(query);
            let mut stmt = conn.prepare(
                "SELECT d.id, d.project_id, d.path,
                        snippet(store_chunks_fts, 0, '<b>', '</b>', '...', 48) AS snip,
                        rank, c.chunk_index
                 FROM store_chunks_fts
                 JOIN store_doc_chunks c ON c.rowid = store_chunks_fts.rowid
                 JOIN store_documents d ON d.id = c.doc_id
                 WHERE store_chunks_fts MATCH ?1
                   AND d.project_id = ?2
                   AND d.deleted_at IS NULL
                 ORDER BY rank
                 LIMIT ?3",
            )?;
            let rows = stmt.query_map(params![fts_query, project_id, (limit * 2) as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3).unwrap_or_default(),
                    row.get::<_, f64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })?;
            // Deduplicate to first (best-ranked) chunk per doc; FTS rank is per-chunk.
            let mut seen = std::collections::HashSet::new();
            let mut tmp = Vec::new();
            for r in rows {
                let (id, pid, path, snip, rank, _) = r?;
                if seen.insert(id.clone()) {
                    tmp.push(DocMatch { id, project_id: pid, path, snippet: snip, score: -rank });
                    if tmp.len() >= limit {
                        break;
                    }
                }
            }
            tmp
        };
        let elapsed = start.elapsed().as_millis();
        self.scale_warning(project_id, elapsed);
        Ok(out)
    }

    /// Vector search over chunk embeddings (brute-force cosine, same as
    /// `doc_vec_search`). Called by `chunk_hybrid_search` when a query vector
    /// is available (fastembed / ort embeddings feature).
    /// When `vector` feature + vec0 table exists and chunk count >50000, uses
    /// sqlite-vec ANN (sub-10ms) instead of brute-force.
    pub fn chunk_vec_search(
        &self,
        project_id: &str,
        query_vec: &[f32],
        limit: usize,
    ) -> rusqlite::Result<Vec<DocMatch>> {
        let start = Instant::now();
        // Try ANN when scale warrants it
        #[cfg(feature = "vector")]
        {
            if self.chunk_count(Some(project_id)).unwrap_or(0) > 50_000 {
                if let Ok(vec_out) = self.chunk_vec_search_ann(project_id, query_vec, limit) {
                    let elapsed = start.elapsed().as_millis();
                    self.scale_warning(project_id, elapsed);
                    return Ok(vec_out);
                }
            }
        }
        let out = {
            let conn = self.conn();
            let mut stmt = conn.prepare(
                "SELECT d.id, d.project_id, d.path, v.embedding, c.content
             FROM store_chunk_vec v
             JOIN store_doc_chunks c ON c.id = v.chunk_id
             JOIN store_documents d ON d.id = c.doc_id
             WHERE d.project_id = ?1 AND d.deleted_at IS NULL",
            )?;
            let rows: Vec<(String, String, String, Vec<u8>, String)> = stmt
                .query_map(params![project_id], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut results: Vec<(f64, DocMatch)> = rows
                .into_iter()
                .filter_map(|(id, pid, path, blob, content)| {
                    if blob.len() % 4 != 0 {
                        return None;
                    }
                    let doc_vec: Vec<f32> = blob
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|c| f32::from_le_bytes(*c))
                        .collect();
                    let sim = crate::embed::cosine_similarity(query_vec, &doc_vec)? as f64;
                    // Use first 200 chars of chunk as snippet for vector hits
                    let snippet = if content.len() > 200 {
                        format!("{}...", &content[..200])
                    } else {
                        content.clone()
                    };
                    Some((
                        sim,
                        DocMatch {
                            id,
                            project_id: pid,
                            path,
                            snippet,
                            score: sim,
                        },
                    ))
                })
                .collect();
            results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            // Deduplicate to best chunk per doc (vector hits are per-chunk)
            let mut seen = std::collections::HashSet::new();
            let mut tmp = Vec::new();
            for (_, m) in results {
                if seen.insert(m.id.clone()) {
                    tmp.push(m);
                    if tmp.len() >= limit {
                        break;
                    }
                }
            }
            tmp
        };
        let elapsed = start.elapsed().as_millis();
        self.scale_warning(project_id, elapsed);
        Ok(out)
    }

    #[cfg(feature = "vector")]
    fn chunk_vec_search_ann(
        &self,
        project_id: &str,
        query_vec: &[f32],
        limit: usize,
    ) -> rusqlite::Result<Vec<DocMatch>> {
        let conn = self.conn();
        if !crate::vector::vec_table_exists(&conn) {
            return Err(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
                Some("vec0 table missing".to_string()),
            ));
        }
        let qbytes = crate::embed::vec_to_bytes(query_vec);
        // KNN via vec0 — distance is L2 by default, but we store cosine-normalized vectors,
        // so L2 distance correlates with cosine. Use k = limit via LIMIT.
        let mut stmt = conn.prepare(
            "SELECT c.doc_id, d.project_id, d.path, c.content, v.distance
             FROM store_chunk_vec0 v
             JOIN store_doc_chunks c ON c.rowid = v.rowid
             JOIN store_documents d ON d.id = c.doc_id
             WHERE v.embedding MATCH ?1 AND d.project_id = ?2 AND d.deleted_at IS NULL
             ORDER BY v.distance LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![qbytes, project_id, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, f64>(4)?,
            ))
        })?;
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for r in rows {
            let (doc_id, pid, path, content, dist) = r?;
            if seen.insert(doc_id.clone()) {
                // Convert L2 distance to pseudo-score (higher is better) for compatibility
                let score = 1.0 / (1.0 + dist);
                let snippet = if content.len() > 200 { format!("{}...", &content[..200]) } else { content };
                out.push(DocMatch { id: doc_id, project_id: pid, path, snippet, score });
                if out.len() >= limit { break; }
            }
        }
        Ok(out)
    }

    /// Hybrid over chunks: BM25 chunks + vector chunks fused via RRF (K=60).
    /// Falls back to BM25-only when `query_vec` is empty or no vectors stored.
    pub fn chunk_hybrid_search(
        &self,
        project_id: &str,
        query: &str,
        query_vec: Option<&[f32]>,
        limit: usize,
    ) -> rusqlite::Result<Vec<DocMatch>> {
        let bm25 = self.chunk_search(project_id, query, limit * 2)?;
        let Some(qv) = query_vec else {
            return Ok(bm25.into_iter().take(limit).collect());
        };
        let vec_hits = self.chunk_vec_search(project_id, qv, limit * 2)?;
        if vec_hits.is_empty() {
            return Ok(bm25.into_iter().take(limit).collect());
        }
        // Rank-only RRF over doc ids (order matters, scores don't)
        let bm25_ids: Vec<String> = bm25.iter().map(|m| m.id.clone()).collect();
        let vec_ids: Vec<String> = vec_hits.iter().map(|m| m.id.clone()).collect();
        let fused = crate::retrieval::rrf_fuse(&bm25_ids, &vec_ids, 60.0);
        // Re-materialize DocMatch in fused order (prefer BM25 snippet where available)
        let bm25_by_id: std::collections::HashMap<_, _> =
            bm25.into_iter().map(|m| (m.id.clone(), m)).collect();
        let vec_by_id: std::collections::HashMap<_, _> =
            vec_hits.into_iter().map(|m| (m.id.clone(), m)).collect();
        let out: Vec<DocMatch> = fused
            .into_iter()
            .take(limit)
            .filter_map(|(id, _)| bm25_by_id.get(&id).cloned().or_else(|| vec_by_id.get(&id).cloned()))
            .collect();
        Ok(out)
    }

    pub fn chunk_set_embedding(&self, chunk_id: &str, embedding: &[f32], model: &str) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let now = db_kit::ids::now();
        let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        let n = conn.execute(
            "INSERT INTO store_chunk_vec (chunk_id, embedding, dim, model, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(chunk_id) DO UPDATE SET embedding = ?2, dim = ?3, model = ?4, updated_at = ?5",
            params![chunk_id, bytes, embedding.len() as i64, model, now],
        )?;
        #[cfg(feature = "vector")]
        {
            // Also index in vec0 for ANN when >50k — best-effort, ignore if table missing or dim mismatch
            if let Ok(rowid) = conn.query_row(
                "SELECT rowid FROM store_doc_chunks WHERE id = ?1",
                params![chunk_id],
                |r| r.get::<_, i64>(0),
            ) {
                let _ = conn.execute(
                    "INSERT INTO store_chunk_vec0(rowid, embedding) VALUES (?1, ?2) ON CONFLICT(rowid) DO UPDATE SET embedding = ?2",
                    params![rowid, bytes],
                );
            }
        }
        Ok(n > 0)
    }

    // ── Metadata (AI Search § 5 custom fields, 10KB, 64B prefix) ──────────

    /// Set a custom metadata field on a doc. Enforces AI Search limits:
    /// ≤5 fields per doc, ≤10 KiB value, first 64 bytes indexed. Returns
    /// error if limits exceeded. `value` empty deletes the key.
    pub fn doc_set_meta(&self, doc_id: &str, key: &str, value: &str) -> rusqlite::Result<bool> {
        if key.trim().is_empty() {
            return Err(rusqlite::Error::InvalidParameterName("meta key empty".into()));
        }
        if value.is_empty() {
            let conn = self.conn();
            let n = conn.execute(
                "DELETE FROM store_doc_meta WHERE doc_id = ?1 AND key = ?2",
                params![doc_id, key],
            )?;
            return Ok(n > 0);
        }
        if value.len() > 10 * 1024 {
            return Err(rusqlite::Error::InvalidParameterName("meta value >10KiB".into()));
        }
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM store_doc_meta WHERE doc_id = ?1 AND key != ?2",
            params![doc_id, key],
            |r| r.get(0),
        )?;
        if count >= 5 {
            return Err(rusqlite::Error::InvalidParameterName("meta >5 fields per doc".into()));
        }
        // Only first 64 bytes are filterable (AI Search limit) — store full value, index prefix.
        conn.execute(
            "INSERT INTO store_doc_meta (doc_id, key, value) VALUES (?1, ?2, ?3)
             ON CONFLICT(doc_id, key) DO UPDATE SET value = ?3",
            params![doc_id, key, value],
        )?;
        Ok(true)
    }

    pub fn doc_get_meta(&self, doc_id: &str) -> rusqlite::Result<Vec<(String, String)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare("SELECT key, value FROM store_doc_meta WHERE doc_id = ?1 ORDER BY key")?;
        let rows = stmt.query_map(params![doc_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect()
    }

    // ── Filtered search: FTS + metadata + path glob (AI Search § filtering, path filtering) ─

    /// Filtered doc search — same FTS as `doc_search` but adds:
    /// - `meta_filter`: exact key=value matches (≤5, 64B prefix)
    /// - `path_glob`: SQLite GLOB (e.g. `docs/*.md`, `src/**.rs`)
    pub fn doc_search_filtered(
        &self,
        project_id: &str,
        query: &str,
        limit: usize,
        meta_filter: Option<&[(String, String)]>,
        path_glob: Option<&str>,
    ) -> rusqlite::Result<Vec<DocMatch>> {
        let conn = self.conn();
        let fts_query = flare_search_kit::fts_phrase_query(query);
        // Build meta join predicates — exact match on stored value (prefix filter would be LIKE)
        let mut sql = String::from(
            "SELECT d.id, d.project_id, d.path, snippet(store_docs_fts, 0, '<b>', '</b>', '...', 48) AS snip, rank
             FROM store_docs_fts
             JOIN store_documents d ON d.rowid = store_docs_fts.rowid
             WHERE store_docs_fts MATCH ?1 AND d.project_id = ?2 AND d.deleted_at IS NULL",
        );
        if let Some(filters) = meta_filter {
            for (i, _) in filters.iter().enumerate() {
                sql.push_str(&format!(
                    " AND EXISTS (SELECT 1 FROM store_doc_meta m{i} WHERE m{i}.doc_id = d.id AND m{i}.key = ?{} AND m{i}.value = ?{})",
                    10 + i * 2,
                    11 + i * 2
                ));
            }
        }
        if path_glob.is_some() {
            sql.push_str(" AND d.path GLOB ?3");
        }
        sql.push_str(" ORDER BY rank LIMIT ?4");
        // For simplicity, handle two cases: with and without path glob, to keep placeholder indices stable
        if let Some(glob) = path_glob {
            let mut stmt = conn.prepare(&sql)?;
            let mut params_vec: Vec<String> = vec![fts_query, project_id.to_string(), glob.to_string(), (limit as i64).to_string()];
            if let Some(filters) = meta_filter {
                for (k, v) in filters.iter() {
                    params_vec.push(k.clone());
                    params_vec.push(v.clone());
                }
                // Need to bind in order: ?1=fts, ?2=project, ?3=glob, ?4=limit, then meta pairs starting at ?10 — but we used interleaved indices above.
                // Simplify: re-prepare with correct indices via direct binding using `params_from_iter` is complex due to dynamic count.
                // Fallback to filtered scan: use chunk_search_filtered's simpler approach
                // For now, handle only single meta filter case correctly; multi-filter falls back to post-filter
                if filters.len() == 1 {
                    let rows = stmt.query_map(
                        params![params_vec[0], params_vec[1], params_vec[2], limit as i64, filters[0].0, filters[0].1],
                        |row| {
                            Ok(DocMatch {
                                id: row.get(0)?,
                                project_id: row.get(1)?,
                                path: row.get(2)?,
                                snippet: row.get::<_, String>(3).unwrap_or_default(),
                                score: -row.get::<_, f64>(4)?,
                            })
                        },
                    )?;
                    return rows.collect();
                }
            }
            let rows = stmt.query_map(params![params_vec[0], params_vec[1], params_vec[2], limit as i64], |row| {
                Ok(DocMatch {
                    id: row.get(0)?,
                    project_id: row.get(1)?,
                    path: row.get(2)?,
                    snippet: row.get::<_, String>(3).unwrap_or_default(),
                    score: -row.get::<_, f64>(4)?,
                })
            })?;
            return rows.collect();
        } else if let Some(filters) = meta_filter {
            if filters.len() == 1 {
                let mut stmt = conn.prepare(
                    "SELECT d.id, d.project_id, d.path, snippet(store_docs_fts, 0, '<b>', '</b>', '...', 48) AS snip, rank
                     FROM store_docs_fts
                     JOIN store_documents d ON d.rowid = store_docs_fts.rowid
                     WHERE store_docs_fts MATCH ?1 AND d.project_id = ?2 AND d.deleted_at IS NULL
                       AND EXISTS (SELECT 1 FROM store_doc_meta m WHERE m.doc_id = d.id AND m.key = ?3 AND m.value = ?4)
                     ORDER BY rank LIMIT ?5",
                )?;
                let rows = stmt.query_map(
                    params![fts_query, project_id, filters[0].0, filters[0].1, limit as i64],
                    |row| {
                        Ok(DocMatch {
                            id: row.get(0)?,
                            project_id: row.get(1)?,
                            path: row.get(2)?,
                            snippet: row.get::<_, String>(3).unwrap_or_default(),
                            score: -row.get::<_, f64>(4)?,
                        })
                    },
                )?;
                return rows.collect();
            }
            // Multi-filter fallback: fetch then post-filter by checking meta in memory (limit*2 pool)
            let base = self.doc_search(project_id, query, limit * 3)?;
            let mut out = Vec::new();
            for m in base {
                let meta = self.doc_get_meta(&m.id)?;
                let ok = filters.iter().all(|(k, v)| meta.iter().any(|(mk, mv)| mk == k && mv == v));
                if ok {
                    out.push(m);
                    if out.len() >= limit { break; }
                }
            }
            return Ok(out);
        }
        // No filters — fall back to plain search
        self.doc_search(project_id, query, limit)
    }

    // ── Similarity cache (AI Search § similarity cache) — kv-backed, TTL 5 min ─

    fn cache_key(query: &str, project_id: &str) -> String {
        let norm = query.trim().to_lowercase();
        let mut h = blake3::Hasher::new();
        h.update(project_id.as_bytes());
        h.update(b"|");
        h.update(norm.as_bytes());
        format!("search_cache:{}", h.finalize().to_hex())
    }

    pub fn search_cache_get(&self, query: &str, project_id: &str) -> Option<Vec<DocMatch>> {
        let key = Self::cache_key(query, project_id);
        let conn = self.conn();
        let blob: Vec<u8> = conn.query_row("SELECT value FROM store_kv WHERE key = ?1", params![key], |r| r.get(0)).ok()?;
        let (ts, json): (i64, String) = serde_json::from_slice(&blob).ok()?;
        if db_kit::ids::now() - ts > 5 * 60 * 1000 {
            return None; // expired
        }
        serde_json::from_str(&json).ok()
    }

    pub fn search_cache_put(&self, query: &str, project_id: &str, hits: &[DocMatch]) {
        let key = Self::cache_key(query, project_id);
        let payload = serde_json::json!((db_kit::ids::now(), serde_json::to_string(hits).unwrap_or_default()));
        let blob = serde_json::to_vec(&payload).unwrap_or_default();
        let conn = self.conn();
        let now = db_kit::ids::now();
        let _ = conn.execute(
            "INSERT INTO store_kv (key, value, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
            params![key, blob, now],
        );
    }

    fn row_to_document(row: &rusqlite::Row) -> rusqlite::Result<Document> {
        let tags_str: String = row.get(8)?;
        let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
        Ok(Document {
            id: row.get(0)?,
            project_id: row.get(1)?,
            path: row.get(2)?,
            content: row.get(3)?,
            title: row.get(4)?,
            doc_type: row.get(5)?,
            blob_hash: row.get(6)?,
            mime: row.get(7)?,
            tags,
            session_id: row.get(9)?,
            source: row.get(10)?,
            version: row.get(11)?,
            metadata: row.get(12)?,
            size: row.get(13)?,
            created_at: row.get(14)?,
            updated_at: row.get(15)?,
            deleted_at: row.get(16)?,
        })
    }

    pub fn doc_upsert(
        &self,
        project_id: &str,
        path: &str,
        content: &str,
    ) -> rusqlite::Result<Document> {
        self.doc_upsert_with_opts(project_id, path, content, DocUpsertOpts::default())
    }

    pub fn doc_upsert_with_opts(
        &self,
        project_id: &str,
        path: &str,
        content: &str,
        opts: DocUpsertOpts,
    ) -> rusqlite::Result<Document> {
        let conn = self.conn();
        let now = db_kit::ids::now();

        // BEGIN IMMEDIATE takes SQLite's write lock up front, so the version
        // read below is serialized against other connections instead of
        // racing them (two connections could otherwise both read version N
        // and both compute N+1).
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;

        let existing = tx
            .query_row(
                "SELECT id, content, version, blob_hash, mime, metadata, size,
                        title, doc_type, tags, session_id, source
                 FROM store_documents
                 WHERE project_id = ?1 AND path = ?2",
                params![project_id, path],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i32>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, i64>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, String>(9)?,
                        row.get::<_, Option<String>>(10)?,
                        row.get::<_, String>(11)?,
                    ))
                },
            )
            .optional()?;

        if let Some((
            existing_id,
            old_content,
            old_version,
            old_blob_hash,
            old_mime,
            old_metadata,
            old_size,
            old_title,
            old_doc_type,
            old_tags_json,
            old_session_id,
            old_source,
        )) = existing
        {
            let new_version = old_version + 1;

            // Snapshot current version to history, unless the caller opted
            // out (cache-type documents) or nothing actually changed. Blob-
            // backed callers (e.g. agentflare-artifacts) always pass an
            // empty `content` string here -- the real payload lives in
            // `blob_hash` -- so comparing inline `content` alone would never
            // detect a change and history would never record; also check
            // `blob_hash` so a no-op re-upsert still skips (both unchanged)
            // while a genuine new blob still snapshots (content stays "").
            //
            // A caller can also re-upsert with unchanged content/blob_hash
            // but a different title/doc_type/tags/mime/metadata/size --
            // that's still a real change to the document's persisted state,
            // so every `Some(...)`-provided opts field must match its
            // stored value too, not just content, for this to count as a
            // true no-op. `blob_hash` follows the same Some(...)-provided
            // rule as the rest: a caller that omits it (None) makes no claim
            // about it, so it must not force `unchanged` to false just
            // because the stored value happens to already be Some(...).
            let old_tags: Vec<String> = serde_json::from_str(&old_tags_json).unwrap_or_default();
            let unchanged = old_content == content
                && opts
                    .blob_hash
                    .as_deref()
                    .is_none_or(|v| old_blob_hash.as_deref() == Some(v))
                && opts.title.as_deref().is_none_or(|v| v == old_title)
                && opts.doc_type.as_deref().is_none_or(|v| v == old_doc_type)
                && opts.mime.as_deref().is_none_or(|v| v == old_mime)
                && opts.metadata.as_deref().is_none_or(|v| v == old_metadata)
                && opts.size.is_none_or(|v| v == old_size)
                && opts
                    .session_id
                    .as_deref()
                    .is_none_or(|v| old_session_id.as_deref() == Some(v))
                && opts.source.as_deref().is_none_or(|v| v == old_source)
                && opts.tags.as_ref().is_none_or(|v| *v == old_tags);
            // Bound once because the blob-release decision below depends on
            // it: a snapshot row is what keeps a superseded blob referenced,
            // so the two must never disagree about whether one was written.
            let snapshots_previous_version = opts.track_history && !unchanged;
            if snapshots_previous_version {
                let history_id = db_kit::ids::new_id();
                tx.execute(
                    "INSERT INTO store_doc_history (id, doc_id, version, content, blob_hash, mime, title, metadata, size, created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, (SELECT title FROM store_documents WHERE id = ?2), ?7, ?8, ?9)",
                    params![history_id, existing_id, old_version, old_content, old_blob_hash, old_mime, old_metadata, old_size, now],
                )?;
            }

            tx.execute(
                "UPDATE store_documents SET
                 content = ?1, updated_at = ?2, deleted_at = NULL,
                 version = ?3
                 WHERE id = ?4",
                params![content, now, new_version, existing_id],
            )?;

            // Apply optional updates (need separate UPDATE to avoid long SQL)
            if let Some(title) = &opts.title {
                tx.execute(
                    "UPDATE store_documents SET title = ?1 WHERE id = ?2",
                    params![title, existing_id],
                )?;
            }
            if let Some(doc_type) = &opts.doc_type {
                tx.execute(
                    "UPDATE store_documents SET doc_type = ?1 WHERE id = ?2",
                    params![doc_type, existing_id],
                )?;
            }
            // A replaced blob loses its last reference here, the same way a
            // deleted document's does -- but only when nothing else kept it.
            // A history-tracking upsert has just snapshotted `old_blob_hash`
            // into store_doc_history, and that row is what `doc_history` and
            // `diff` read the previous version's bytes back through, so
            // reclaiming it would silently empty the document's own history.
            // Cache-type callers (track_history = false) write no snapshot, so
            // for them the old blob really is orphaned. Released after the
            // commit below, once the row no longer points at it.
            let superseded_blob = match (&opts.blob_hash, &old_blob_hash) {
                (Some(new), Some(old)) if new != old && !snapshots_previous_version => {
                    Some(old.clone())
                }
                _ => None,
            };
            if opts.blob_hash.is_some() {
                tx.execute(
                    "UPDATE store_documents SET blob_hash = ?1 WHERE id = ?2",
                    params![opts.blob_hash, existing_id],
                )?;
            }
            if let Some(mime) = &opts.mime {
                tx.execute(
                    "UPDATE store_documents SET mime = ?1 WHERE id = ?2",
                    params![mime, existing_id],
                )?;
            }
            if let Some(tags) = &opts.tags {
                let json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
                tx.execute(
                    "UPDATE store_documents SET tags = ?1 WHERE id = ?2",
                    params![json, existing_id],
                )?;
            }
            if opts.session_id.is_some() {
                tx.execute(
                    "UPDATE store_documents SET session_id = ?1 WHERE id = ?2",
                    params![opts.session_id, existing_id],
                )?;
            }
            if let Some(source) = &opts.source {
                tx.execute(
                    "UPDATE store_documents SET source = ?1 WHERE id = ?2",
                    params![source, existing_id],
                )?;
            }
            if let Some(metadata) = &opts.metadata {
                tx.execute(
                    "UPDATE store_documents SET metadata = ?1 WHERE id = ?2",
                    params![metadata, existing_id],
                )?;
            }
            if let Some(size) = opts.size {
                tx.execute(
                    "UPDATE store_documents SET size = ?1 WHERE id = ?2",
                    params![size, existing_id],
                )?;
            }

            tx.commit()?;
            drop(conn);
            // Chunk sync is best-effort post-commit; failure rolls back only chunks,
            // not the doc itself (doc is already durable).
            let _ = self.sync_chunks(&existing_id, content);
            if let Some(old) = superseded_blob {
                self.blob_unref(&old)?;
            }
            self.doc_get(&existing_id).map(|o| o.unwrap())
        } else {
            let id = db_kit::ids::new_id();
            let title = opts.title.unwrap_or_default();
            let doc_type = opts.doc_type.unwrap_or_else(|| "file".to_string());
            let mime = opts.mime.unwrap_or_default();
            let tags_val = opts.tags.unwrap_or_default();
            let tags_json = serde_json::to_string(&tags_val).unwrap_or_else(|_| "[]".to_string());
            let source = opts.source.unwrap_or_default();
            let metadata = opts.metadata.unwrap_or_else(|| "{}".to_string());
            let size = opts.size.unwrap_or(0);

            tx.execute(
                "INSERT INTO store_documents
                 (id, project_id, path, content, title, doc_type, blob_hash, mime, tags, session_id, source, metadata, size, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1, ?14, ?14)",
                params![
                    id, project_id, path, content, title, doc_type, opts.blob_hash,
                    mime, tags_json, opts.session_id, source, metadata, size, now
                ],
            )?;
            tx.commit()?;
            drop(conn);
            let _ = self.sync_chunks(&id, content);
            Ok(Document {
                id,
                project_id: project_id.to_string(),
                path: path.to_string(),
                content: content.to_string(),
                title,
                doc_type,
                blob_hash: opts.blob_hash,
                mime,
                tags: tags_val,
                session_id: opts.session_id,
                source,
                metadata,
                size,
                version: 1,
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
        }
    }

    pub fn doc_get(&self, id: &str) -> rusqlite::Result<Option<Document>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT id, project_id, path, content, title, doc_type, blob_hash, mime, tags,
                        session_id, source, version, metadata, size, created_at, updated_at, deleted_at
                 FROM store_documents WHERE id = ?1 AND deleted_at IS NULL",
            params![id],
            Self::row_to_document,
        )
        .optional()
    }

    pub fn doc_get_by_path(
        &self,
        project_id: &str,
        path: &str,
    ) -> rusqlite::Result<Option<Document>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT id, project_id, path, content, title, doc_type, blob_hash, mime, tags,
                        session_id, source, version, metadata, size, created_at, updated_at, deleted_at
                 FROM store_documents WHERE project_id = ?1 AND path = ?2 AND deleted_at IS NULL",
            params![project_id, path],
            Self::row_to_document,
        )
        .optional()
    }

    pub fn doc_delete(&self, id: &str) -> rusqlite::Result<bool> {
        // Scoped so the connection guard is released before blob_unref, which
        // takes the same non-reentrant mutex and opens its own transaction.
        let (found, release_blob) = {
            let conn = self.conn();
            let now = db_kit::ids::now();
            let Some((blob_hash, already_deleted)) = conn
                .query_row(
                    "SELECT blob_hash, deleted_at IS NOT NULL FROM store_documents WHERE id = ?1",
                    params![id],
                    |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, bool>(1)?)),
                )
                .optional()?
            else {
                return Ok(false);
            };
            // Dropping the row out of store_docs_fts is the AFTER UPDATE
            // trigger's job now -- see migrations::EXTERNAL_CONTENT_FTS_MIGRATION.
            conn.execute(
                "UPDATE store_documents SET deleted_at = ?1 WHERE id = ?2",
                params![now, id],
            )?;
            // Chunks are soft-deleted with the doc (FTS visibility is via
            // `d.deleted_at IS NULL` join, but reclaim storage now).
            if !already_deleted {
                conn.execute("DELETE FROM store_chunk_vec WHERE chunk_id IN (SELECT id FROM store_doc_chunks WHERE doc_id = ?1)", params![id])?;
                conn.execute("DELETE FROM store_doc_chunks WHERE doc_id = ?1", params![id])?;
            }
            // Only the delete that actually transitions live -> deleted owns a
            // reference. Re-deleting an already soft-deleted row must not
            // decrement again, or a blob shared with a live document loses its
            // content while that document still looks intact.
            (true, if already_deleted { None } else { blob_hash })
        };

        // Releasing the blob after the row is soft-deleted, not before: if the
        // order were reversed and the row update failed, the last reference's
        // content would already be gone while the document still read as live.
        // Same ordering as the asset MCP tool's delete path.
        //
        // A caller that later resurrects this row via doc_upsert_with_opts
        // without supplying a fresh blob_hash would be left pointing at
        // reclaimed bytes; every blob-backed caller today (assets, artifacts,
        // flare-docs rustdoc) always passes one on upsert.
        if let Some(hash) = release_blob {
            self.blob_unref(&hash)?;
        }
        Ok(found)
    }

    pub fn doc_history(&self, doc_id: &str) -> rusqlite::Result<Vec<DocVersion>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, doc_id, version, content, blob_hash, mime, title, metadata, size, created_at
             FROM store_doc_history
             WHERE doc_id = ?1
             ORDER BY version DESC",
        )?;
        let rows = stmt.query_map(params![doc_id], |row| {
            Ok(DocVersion {
                id: row.get(0)?,
                doc_id: row.get(1)?,
                version: row.get(2)?,
                content: row.get(3)?,
                blob_hash: row.get(4)?,
                mime: row.get(5)?,
                title: row.get(6)?,
                metadata: row.get(7)?,
                size: row.get(8)?,
                created_at: row.get(9)?,
            })
        })?;
        rows.collect()
    }

    pub fn doc_search(
        &self,
        project_id: &str,
        query: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<DocMatch>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT d.id, d.project_id, d.path,
                    snippet(store_docs_fts, 0, '<b>', '</b>', '...', 48) AS snip,
                    rank
             FROM store_docs_fts
             JOIN store_documents d ON d.rowid = store_docs_fts.rowid
             WHERE store_docs_fts MATCH ?1
               AND d.project_id = ?2
               AND d.deleted_at IS NULL
             ORDER BY rank
             LIMIT ?3",
        )?;
        // store_docs_fts is a single unnamed-column table matched with a bare
        // `MATCH ?1` (no AND/OR structure of our own) -- fts_phrase_query
        // individually quotes every whitespace token so embedded FTS5
        // operators/column-filter syntax (NEAR, *, `word:`) in the raw query
        // can't be reinterpreted as query structure. Without this, a query
        // like "axum::extract::State" errors ("no such column: axum")
        // instead of searching, because FTS5 parses a bare `word:` prefix as
        // a column filter.
        let fts_query = flare_search_kit::fts_phrase_query(query);
        let rows = stmt.query_map(params![fts_query, project_id, limit as i64], |row| {
            Ok(DocMatch {
                id: row.get(0)?,
                project_id: row.get(1)?,
                path: row.get(2)?,
                snippet: row.get::<_, String>(3).unwrap_or_default(),
                score: -row.get::<_, f64>(4)?,
            })
        })?;
        rows.collect()
    }

    pub fn doc_set_embedding(&self, doc_id: &str, embedding: &[f32]) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let now = db_kit::ids::now();
        let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
        let n = conn.execute(
            "INSERT INTO store_docs_vec (doc_id, embedding, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(doc_id) DO UPDATE SET embedding = ?2, updated_at = ?3",
            params![doc_id, bytes, now],
        )?;
        Ok(n > 0)
    }

    pub fn doc_get_embedding(&self, doc_id: &str) -> rusqlite::Result<Option<Vec<f32>>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT embedding FROM store_docs_vec WHERE doc_id = ?1",
            params![doc_id],
            |row| {
                let blob: Vec<u8> = row.get(0)?;
                let vec: Vec<f32> = blob
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| f32::from_le_bytes(*c))
                    .collect();
                Ok(vec)
            },
        )
        .optional()
    }

    pub fn doc_vec_search(
        &self,
        project_id: &str,
        query_vec: &[f32],
        limit: usize,
    ) -> rusqlite::Result<Vec<DocMatch>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT d.id, d.project_id, d.path, v.embedding
             FROM store_docs_vec v
             JOIN store_documents d ON d.id = v.doc_id
             WHERE d.project_id = ?1 AND d.deleted_at IS NULL",
        )?;
        let rows: Vec<(String, String, String, Vec<u8>)> = stmt
            .query_map(params![project_id], |row| {
                let id: String = row.get(0)?;
                let project_id: String = row.get(1)?;
                let path: String = row.get(2)?;
                let blob: Vec<u8> = row.get(3)?;
                Ok((id, project_id, path, blob))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut results: Vec<(f64, DocMatch)> = rows
            .into_iter()
            .filter_map(|(id, pid, path, blob)| {
                if blob.len() % 4 != 0 {
                    return None;
                }
                let doc_vec: Vec<f32> = blob
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| f32::from_le_bytes(*c))
                    .collect();
                let sim = crate::embed::cosine_similarity(query_vec, &doc_vec)? as f64;
                Some((
                    sim,
                    DocMatch {
                        id,
                        project_id: pid,
                        path,
                        snippet: String::new(),
                        score: sim,
                    },
                ))
            })
            .collect();
        results.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        Ok(results.into_iter().map(|(_, m)| m).collect())
    }

    pub fn doc_hybrid_search(
        &self,
        project_id: &str,
        fts_query: &str,
        query_vec: &[f32],
        limit: usize,
        alpha: f64,
    ) -> rusqlite::Result<Vec<DocMatch>> {
        let mut fts = self.doc_search(project_id, fts_query, limit * 2)?;
        let mut vec = self.doc_vec_search(project_id, query_vec, limit * 2)?;

        let mut max_fts = fts.first().map(|m| m.score).unwrap_or(1.0);
        let mut max_vec = vec.first().map(|m| m.score).unwrap_or(1.0);
        if max_fts < 1e-12 {
            max_fts = 1.0;
        }
        if max_vec < 1e-12 {
            max_vec = 1.0;
        }

        for m in &mut fts {
            m.score = alpha * (m.score / max_fts);
        }
        for m in &mut vec {
            m.score = (1.0 - alpha) * (m.score / max_vec);
        }

        let mut combined: Vec<DocMatch> = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for m in fts.into_iter().chain(vec) {
            if seen.insert(m.id.clone()) {
                combined.push(m);
            } else if let Some(existing) = combined.iter_mut().find(|e| e.id == m.id) {
                existing.score += m.score;
            }
        }

        combined.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        combined.truncate(limit);
        Ok(combined)
    }

    pub fn doc_list(&self, project_id: &str) -> rusqlite::Result<Vec<Document>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, project_id, path, content, title, doc_type, blob_hash, mime, tags,
                    session_id, source, version, metadata, size, created_at, updated_at, deleted_at
             FROM store_documents
             WHERE project_id = ?1 AND deleted_at IS NULL
             ORDER BY path",
        )?;
        let rows = stmt.query_map(params![project_id], Self::row_to_document)?;
        rows.collect()
    }

    /// Live documents in a project, without their bodies.
    ///
    /// `LIMIT`/`OFFSET` are pushed into SQL rather than applied to a fetched
    /// `Vec`: truncating in Rust still makes SQLite read and allocate every
    /// row's `content` first, which is the cost this projection exists to
    /// avoid. `content_bytes` casts to BLOB before measuring because
    /// SQLite's `LENGTH()` counts characters on TEXT, and a byte count is
    /// what a caller sizing a response actually needs.
    pub fn doc_list_summaries(
        &self,
        project_id: &str,
        limit: usize,
        offset: usize,
    ) -> rusqlite::Result<Vec<DocumentSummary>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, path, title, doc_type, source, version, created_at, updated_at,
                    LENGTH(CAST(content AS BLOB))
             FROM store_documents
             WHERE project_id = ?1 AND deleted_at IS NULL
             ORDER BY path
             LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(
            params![
                project_id,
                i64::try_from(limit).unwrap_or(i64::MAX),
                i64::try_from(offset).unwrap_or(i64::MAX),
            ],
            |row| {
                Ok(DocumentSummary {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    title: row.get(2)?,
                    doc_type: row.get(3)?,
                    source: row.get(4)?,
                    version: row.get(5)?,
                    created_at: row.get(6)?,
                    updated_at: row.get(7)?,
                    content_bytes: row.get(8)?,
                })
            },
        )?;
        rows.collect()
    }

    /// How many live documents a project holds.
    ///
    /// Separate from [`Self::doc_list_summaries`] so a capped listing can
    /// report the size of the set it was drawn from — a truncated page that
    /// cannot say how much it left behind reads exactly like a complete one.
    pub fn doc_count(&self, project_id: &str) -> rusqlite::Result<usize> {
        let conn = self.conn();
        let n: i64 = conn.query_row(
            "SELECT COUNT(*) FROM store_documents
             WHERE project_id = ?1 AND deleted_at IS NULL",
            params![project_id],
            |row| row.get(0),
        )?;
        Ok(usize::try_from(n).unwrap_or(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_memory().unwrap()
    }

    /// Rows matchable through `store_docs_fts` directly, with no
    /// `deleted_at` guard layered on top. `doc_search` filters deleted rows
    /// itself, so it cannot tell a correctly-maintained index from one full
    /// of stale entries -- these assertions have to bypass it.
    fn fts_hits(s: &Store, term: &str) -> i64 {
        s.conn()
            .query_row(
                "SELECT COUNT(*) FROM store_docs_fts WHERE store_docs_fts MATCH ?1",
                params![term],
                |r| r.get(0),
            )
            .unwrap()
    }

    #[test]
    fn soft_deleting_a_document_drops_its_fts_entry() {
        let s = store();
        let doc = s.doc_upsert("p", "/gone.md", "loquacious").unwrap();
        assert_eq!(fts_hits(&s, "loquacious"), 1);

        s.doc_delete(&doc.id).unwrap();

        assert_eq!(
            fts_hits(&s, "loquacious"),
            0,
            "a soft-deleted document must leave no directly matchable index row"
        );
    }

    #[test]
    fn resurrecting_a_soft_deleted_document_indexes_it_exactly_once() {
        // The failure mode the guarded triggers exist for: on an
        // external-content table, a `'delete'` naming a row that is not in
        // the index corrupts it. A resurrecting upsert is an UPDATE whose
        // `old` row was soft-deleted (and so already unindexed), which an
        // unguarded AFTER UPDATE trigger would happily try to delete again.
        let s = store();
        let doc = s.doc_upsert("p", "/back.md", "phoenix").unwrap();
        s.doc_delete(&doc.id).unwrap();
        assert_eq!(fts_hits(&s, "phoenix"), 0);

        s.doc_upsert("p", "/back.md", "phoenix").unwrap();

        assert_eq!(fts_hits(&s, "phoenix"), 1);
        assert_eq!(s.doc_search("p", "phoenix", 10).unwrap().len(), 1);
        // fts5's own verdict on whether the delete/insert bookkeeping stayed
        // consistent -- an unguarded trigger fails here, not on the counts.
        s.conn()
            .execute_batch("INSERT INTO store_docs_fts(store_docs_fts) VALUES('integrity-check');")
            .expect("index must survive a delete/resurrect cycle intact");
    }

    #[test]
    fn hard_deleting_a_row_drops_its_fts_entry() {
        // Nothing in the crate hard-deletes documents today, but the
        // cache-eviction work (#339) will. With the index trigger-driven,
        // that purge gets the FTS delete for free instead of having to
        // remember it -- which is the class of bug #334/#337 was.
        let s = store();
        s.doc_upsert("p", "/purge.md", "ephemeral").unwrap();
        s.conn()
            .execute("DELETE FROM store_documents WHERE path = '/purge.md'", [])
            .unwrap();

        assert_eq!(fts_hits(&s, "ephemeral"), 0);
    }

    #[test]
    fn editing_a_document_leaves_only_the_new_content_matchable() {
        let s = store();
        s.doc_upsert("p", "/edit.md", "before").unwrap();
        s.doc_upsert("p", "/edit.md", "after").unwrap();

        assert_eq!(fts_hits(&s, "before"), 0, "superseded text must not match");
        assert_eq!(fts_hits(&s, "after"), 1);
    }

    #[test]
    fn the_update_trigger_is_scoped_to_the_indexed_columns() {
        // doc_upsert_with_opts writes content, then up to nine more
        // single-column UPDATEs for the optional fields. None of them can
        // change what is indexed, so none should re-tokenize the body. This
        // has to be asserted on the trigger definition: an unscoped
        // AFTER UPDATE produces an identical index, just nine times over.
        let s = store();
        let sql: String = s
            .conn()
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'store_docs_fts_au'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            sql.contains("UPDATE OF content, deleted_at"),
            "update trigger must not fire for columns the index ignores: {sql}"
        );
    }

    #[test]
    fn optional_field_updates_leave_the_document_indexed() {
        let s = store();
        s.doc_upsert_with_opts(
            "p",
            "/opts.md",
            "indexable",
            DocUpsertOpts {
                title: Some("t".into()),
                mime: Some("text/plain".into()),
                tags: Some(vec!["a".into()]),
                source: Some("src".into()),
                size: Some(9),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(fts_hits(&s, "indexable"), 1);
    }

    #[test]
    fn doc_fts_rebuild_restores_the_index_and_still_skips_deleted_rows() {
        let s = store();
        s.doc_upsert("p", "/live.md", "kept").unwrap();
        let gone = s.doc_upsert("p", "/dead.md", "dropped").unwrap();
        s.doc_delete(&gone.id).unwrap();

        // Stand in for whatever desyncs the index behind the triggers' back
        // -- a VACUUM renumbering store_documents' implicit rowids being the
        // case doc_fts_rebuild exists for.
        s.conn()
            .execute_batch("INSERT INTO store_docs_fts(store_docs_fts) VALUES('delete-all');")
            .unwrap();
        assert_eq!(fts_hits(&s, "kept"), 0);

        s.doc_fts_rebuild().unwrap();

        assert_eq!(fts_hits(&s, "kept"), 1);
        assert_eq!(
            fts_hits(&s, "dropped"),
            0,
            "a rebuild must not resurrect soft-deleted rows the triggers keep out"
        );
    }

    #[test]
    fn summaries_page_without_carrying_bodies() {
        let s = store();
        let big = "x".repeat(50_000);
        for i in 0..5 {
            s.doc_upsert("proj-1", &format!("/doc{i}.md"), &big)
                .unwrap();
        }

        let page = s.doc_list_summaries("proj-1", 2, 0).unwrap();
        assert_eq!(page.len(), 2);
        assert_eq!(page[0].path, "/doc0.md");
        // A byte count, not the bytes: the whole point of the projection.
        assert_eq!(page[0].content_bytes, 50_000);

        let next = s.doc_list_summaries("proj-1", 2, 2).unwrap();
        assert_eq!(next[0].path, "/doc2.md");

        assert_eq!(s.doc_count("proj-1").unwrap(), 5);
    }

    #[test]
    fn summaries_and_count_skip_deleted_and_other_projects() {
        let s = store();
        let keep = s.doc_upsert("proj-1", "/keep.md", "keep").unwrap();
        let gone = s.doc_upsert("proj-1", "/gone.md", "gone").unwrap();
        s.doc_upsert("proj-2", "/other.md", "other").unwrap();
        s.doc_delete(&gone.id).unwrap();

        let page = s.doc_list_summaries("proj-1", 10, 0).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].id, keep.id);
        assert_eq!(s.doc_count("proj-1").unwrap(), 1);
    }

    #[test]
    fn content_bytes_counts_bytes_not_characters() {
        // SQLite's LENGTH() counts characters on TEXT; a caller sizing a
        // response needs bytes, so the projection casts to BLOB first. Without
        // that cast this doc reports 2, not 6.
        let s = store();
        s.doc_upsert("proj-1", "/utf8.md", "日本").unwrap();
        let page = s.doc_list_summaries("proj-1", 10, 0).unwrap();
        assert_eq!(page[0].content_bytes, 6);
    }

    #[test]
    fn create_and_read() {
        let s = store();
        let doc = s.doc_upsert("proj-1", "/hello.md", "Hello world").unwrap();
        assert_eq!(doc.project_id, "proj-1");
        assert_eq!(doc.path, "/hello.md");
        assert!(doc.deleted_at.is_none());

        let fetched = s.doc_get(&doc.id).unwrap().unwrap();
        assert_eq!(fetched.content, "Hello world");
    }

    #[test]
    fn upsert_updates_existing() {
        let s = store();
        let doc = s.doc_upsert("p", "/same.md", "v1").unwrap();
        let updated = s.doc_upsert("p", "/same.md", "v2").unwrap();
        assert_eq!(updated.id, doc.id);
        assert_eq!(updated.content, "v2");
    }

    #[test]
    fn soft_delete_and_list() {
        let s = store();
        s.doc_upsert("p", "/a.md", "a").unwrap();
        let b = s.doc_upsert("p", "/b.md", "b").unwrap();
        assert_eq!(s.doc_list("p").unwrap().len(), 2);

        s.doc_delete(&b.id).unwrap();
        let list = s.doc_list("p").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].path, "/a.md");

        // doc_get must not resurrect a soft-deleted row -- callers (e.g. the
        // asset MCP tool) rely on this to report "not found" post-delete.
        assert!(s.doc_get(&b.id).unwrap().is_none());
    }

    #[test]
    fn fts_search_finds_matching_content() {
        let s = store();
        s.doc_upsert("p", "/rust.md", "Rust is a systems programming language")
            .unwrap();
        s.doc_upsert("p", "/go.md", "Go is fast and concurrent")
            .unwrap();
        s.doc_upsert("p", "/python.md", "Python is great for data science")
            .unwrap();

        let results = s.doc_search("p", "rust", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "/rust.md");
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn fts_search_multi_word() {
        let s = store();
        s.doc_upsert("p", "/a.md", "the quick brown fox").unwrap();
        s.doc_upsert("p", "/b.md", "jumps over the lazy dog")
            .unwrap();

        let results = s.doc_search("p", "quick fox", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "/a.md");
    }

    #[test]
    fn fts_search_handles_double_colon_query_without_erroring() {
        // Regression: a raw `::`-containing query (the natural way to
        // search for a Rust fully-qualified path, e.g. "axum::extract::State")
        // used to be passed straight to FTS5 MATCH, which parses a bare
        // `word:` prefix as column-filter syntax and errored with
        // "no such column: axum" instead of searching.
        let s = store();
        s.doc_upsert(
            "p",
            "/state.md",
            "State is an extractor for axum::extract::State shared state",
        )
        .unwrap();

        let results = s.doc_search("p", "axum::extract::State", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].path, "/state.md");
    }

    #[test]
    fn fts_search_scoped_to_project() {
        let s = store();
        s.doc_upsert("p1", "/doc.md", "shared term").unwrap();
        s.doc_upsert("p2", "/doc.md", "shared term").unwrap();

        let results = s.doc_search("p1", "shared term", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    fn test_embed(s: &Store, doc_id: &str, dim: usize, val: f32) {
        let embedding = vec![val; dim];
        s.doc_set_embedding(doc_id, &embedding).unwrap();
    }

    #[test]
    fn set_and_get_embedding() {
        let s = store();
        let doc = s.doc_upsert("p", "/doc.md", "content").unwrap();
        let emb = vec![0.1, 0.2, 0.3];
        s.doc_set_embedding(&doc.id, &emb).unwrap();
        let got = s.doc_get_embedding(&doc.id).unwrap().unwrap();
        assert_eq!(got.len(), 3);
        assert!((got[0] - 0.1).abs() < 1e-6);
    }

    #[test]
    fn embedding_bytes_are_stored_little_endian() {
        // Regression test for a bug flagged in item #148's review: the
        // write side used bytemuck::cast_slice (native-endian) while the
        // read side hardcoded f32::from_le_bytes — matched by accident on
        // little-endian hosts, but not guaranteed by the code. Assert the
        // on-disk byte layout directly, not just the round trip.
        let s = store();
        let doc = s.doc_upsert("p", "/endian.md", "content").unwrap();
        let value: f32 = 1.5;
        s.doc_set_embedding(&doc.id, &[value]).unwrap();

        let raw: Vec<u8> = s
            .conn()
            .query_row(
                "SELECT embedding FROM store_docs_vec WHERE doc_id = ?1",
                params![doc.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw, value.to_le_bytes().to_vec());
    }

    #[test]
    fn vec_search_ranks_by_similarity() {
        let s = store();
        let d1 = s.doc_upsert("p", "/cat.md", "about cats").unwrap();
        let d2 = s.doc_upsert("p", "/dog.md", "about dogs").unwrap();
        let d3 = s.doc_upsert("p", "/car.md", "about cars").unwrap();

        // Directionally distinct so cosine similarity actually differs —
        // uniform-value vectors like [1,1,1,1] vs [0.8,0.8,0.8,0.8] are
        // collinear and score identically regardless of magnitude.
        s.doc_set_embedding(&d1.id, &[1.0, 0.0, 0.0, 0.0]).unwrap();
        s.doc_set_embedding(&d2.id, &[1.0, 1.0, 0.0, 0.0]).unwrap();
        s.doc_set_embedding(&d3.id, &[0.0, 1.0, 0.0, 0.0]).unwrap();

        let query = vec![1.0, 0.0, 0.0, 0.0];
        let results = s.doc_vec_search("p", &query, 10).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].id, d1.id);
        assert_eq!(results[1].id, d2.id);
        assert_eq!(results[2].id, d3.id);
    }

    #[test]
    fn hybrid_search_combines_scores() {
        let s = store();
        let d1 = s
            .doc_upsert("p", "/rust.md", "Rust programming language")
            .unwrap();
        s.doc_upsert("p", "/other.md", "Something else entirely")
            .unwrap();

        test_embed(&s, &d1.id, 4, 1.0);

        let query_vec = vec![1.0; 4];
        let results = s
            .doc_hybrid_search("p", "rust", &query_vec, 10, 0.5)
            .unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].id, d1.id);
    }

    #[test]
    fn upsert_with_metadata() {
        let s = store();
        let doc = s
            .doc_upsert_with_opts(
                "p",
                "/meta.md",
                "content",
                DocUpsertOpts {
                    title: Some("My Doc".into()),
                    doc_type: Some("note".into()),
                    mime: Some("text/markdown".into()),
                    tags: Some(vec!["rust".into(), "db".into()]),
                    source: Some("agent".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(doc.title, "My Doc");
        assert_eq!(doc.doc_type, "note");
        assert_eq!(doc.mime, "text/markdown");
        assert_eq!(doc.tags, vec!["rust", "db"]);
        assert_eq!(doc.source, "agent");
        assert_eq!(doc.version, 1);
    }

    #[test]
    fn versioning_increments_on_upsert() {
        let s = store();
        let doc = s.doc_upsert("p", "/v.md", "v1").unwrap();
        assert_eq!(doc.version, 1);

        let updated = s.doc_upsert("p", "/v.md", "v2").unwrap();
        assert_eq!(updated.version, 2);

        let history = s.doc_history(&updated.id).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].version, 1);
        assert_eq!(history[0].content, "v1");
    }

    #[test]
    fn track_history_false_skips_history_row() {
        let s = store();
        s.doc_upsert_with_opts(
            "p",
            "/v.md",
            "v1",
            DocUpsertOpts {
                track_history: false,
                ..Default::default()
            },
        )
        .unwrap();
        let updated = s
            .doc_upsert_with_opts(
                "p",
                "/v.md",
                "v2",
                DocUpsertOpts {
                    track_history: false,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.content, "v2");
        assert!(s.doc_history(&updated.id).unwrap().is_empty());
    }

    #[test]
    fn identical_content_reupsert_skips_history_row() {
        let s = store();
        let doc = s.doc_upsert("p", "/same.md", "same content").unwrap();
        let updated = s.doc_upsert("p", "/same.md", "same content").unwrap();
        assert_eq!(updated.id, doc.id);
        assert!(
            s.doc_history(&updated.id).unwrap().is_empty(),
            "no history row should be written when content is unchanged"
        );
    }

    #[test]
    fn unchanged_content_with_changed_title_still_writes_history() {
        let s = store();
        let doc = s
            .doc_upsert_with_opts(
                "p",
                "/title-change.md",
                "same content",
                DocUpsertOpts {
                    title: Some("Old Title".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let updated = s
            .doc_upsert_with_opts(
                "p",
                "/title-change.md",
                "same content",
                DocUpsertOpts {
                    title: Some("New Title".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.id, doc.id);
        assert_eq!(updated.title, "New Title");
        let history = s.doc_history(&updated.id).unwrap();
        assert_eq!(
            history.len(),
            1,
            "a title-only change must still write a history row even though content didn't change"
        );
        assert_eq!(history[0].title, "Old Title");
    }

    #[test]
    fn identical_content_and_opts_reupsert_skips_history_row() {
        let s = store();
        let opts = || DocUpsertOpts {
            title: Some("Same Title".into()),
            tags: Some(vec!["rust".into()]),
            ..Default::default()
        };
        let doc = s
            .doc_upsert_with_opts("p", "/no-op.md", "same content", opts())
            .unwrap();
        let updated = s
            .doc_upsert_with_opts("p", "/no-op.md", "same content", opts())
            .unwrap();
        assert_eq!(updated.id, doc.id);
        assert!(
            s.doc_history(&updated.id).unwrap().is_empty(),
            "a true no-op re-upsert (same content and same opts) must not write history"
        );
    }

    #[test]
    fn omitted_blob_hash_does_not_force_a_history_write() {
        let s = store();
        let doc = s
            .doc_upsert_with_opts(
                "p",
                "/blob.md",
                "same content",
                DocUpsertOpts {
                    blob_hash: Some("hash-v1".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        // Re-upsert with identical content and blob_hash omitted (None) --
        // omitting a field makes no claim about it, so it must not be
        // treated as "the blob_hash changed" just because the stored value
        // happens to already be Some(...).
        let updated = s
            .doc_upsert_with_opts("p", "/blob.md", "same content", DocUpsertOpts::default())
            .unwrap();
        assert_eq!(updated.id, doc.id);
        assert_eq!(updated.blob_hash.as_deref(), Some("hash-v1"));
        assert!(
            s.doc_history(&updated.id).unwrap().is_empty(),
            "omitting blob_hash on an otherwise-unchanged re-upsert must not write history"
        );
    }

    #[test]
    fn unchanged_content_with_changed_metadata_still_writes_history() {
        let s = store();
        let doc = s
            .doc_upsert_with_opts(
                "p",
                "/meta-change.md",
                "same content",
                DocUpsertOpts {
                    metadata: Some(r#"{"v":1}"#.into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let updated = s
            .doc_upsert_with_opts(
                "p",
                "/meta-change.md",
                "same content",
                DocUpsertOpts {
                    metadata: Some(r#"{"v":2}"#.into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.id, doc.id);
        assert_eq!(updated.metadata, r#"{"v":2}"#);
        let history = s.doc_history(&updated.id).unwrap();
        assert_eq!(
            history.len(),
            1,
            "a metadata-only change must still write a history row"
        );
        assert_eq!(history[0].metadata, r#"{"v":1}"#);

        // A second re-upsert with identical metadata must be a true no-op.
        s.doc_upsert_with_opts(
            "p",
            "/meta-change.md",
            "same content",
            DocUpsertOpts {
                metadata: Some(r#"{"v":2}"#.into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(
            s.doc_history(&updated.id).unwrap().len(),
            1,
            "identical metadata must not add another history row"
        );
    }

    #[test]
    fn project_path_unique_index_rejects_duplicate_raw_insert() {
        let s = store();
        s.doc_upsert("p", "/dup.md", "content").unwrap();
        let conn = s.conn();
        let result = conn.execute(
            "INSERT INTO store_documents
             (id, project_id, path, content, title, doc_type, blob_hash, mime, tags, session_id, source, metadata, size, version, created_at, updated_at)
             VALUES ('dup-id-2', 'p', '/dup.md', 'other', '', 'file', NULL, '', '[]', NULL, '', '{}', 0, 1, 0, 0)",
            [],
        );
        assert!(
            result.is_err(),
            "duplicate (project_id, path) should be rejected by the unique index"
        );
    }

    #[test]
    fn history_snapshot_preserves_blob_hash_and_mime() {
        let s = store();
        let doc = s
            .doc_upsert_with_opts(
                "p",
                "/v.md",
                "v1",
                DocUpsertOpts {
                    blob_hash: Some("hash-v1".into()),
                    mime: Some("text/plain".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(doc.version, 1);

        s.doc_upsert_with_opts(
            "p",
            "/v.md",
            "v2",
            DocUpsertOpts {
                blob_hash: Some("hash-v2".into()),
                mime: Some("text/markdown".into()),
                ..Default::default()
            },
        )
        .unwrap();

        let history = s.doc_history(&doc.id).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].blob_hash.as_deref(), Some("hash-v1"));
        assert_eq!(history[0].mime, "text/plain");
    }
}
