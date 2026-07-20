use crate::Store;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Serialize)]
pub struct DocMatch {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub snippet: String,
    pub score: f64,
}

#[derive(Debug, Default)]
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
}

impl Store {
    fn doc_sync_fts(
        conn: &rusqlite::Connection,
        row_id: i64,
        content: &str,
    ) -> rusqlite::Result<()> {
        conn.execute(
            "DELETE FROM store_docs_fts WHERE rowid = ?1",
            params![row_id],
        )?;
        conn.execute(
            "INSERT INTO store_docs_fts(rowid, content) VALUES (?1, ?2)",
            params![row_id, content],
        )?;
        Ok(())
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
                "SELECT id, rowid, content, version, blob_hash, mime, metadata, size FROM store_documents
                 WHERE project_id = ?1 AND path = ?2",
                params![project_id, path],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i32>(3)?,
                        row.get::<_, Option<String>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, i64>(7)?,
                    ))
                },
            )
            .optional()?;

        if let Some((existing_id, rowid, old_content, old_version, old_blob_hash, old_mime, old_metadata, old_size)) =
            existing
        {
            let new_version = old_version + 1;
            let history_id = db_kit::ids::new_id();

            // Snapshot current version to history
            tx.execute(
                "INSERT INTO store_doc_history (id, doc_id, version, content, blob_hash, mime, title, metadata, size, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, (SELECT title FROM store_documents WHERE id = ?2), ?7, ?8, ?9)",
                params![history_id, existing_id, old_version, old_content, old_blob_hash, old_mime, old_metadata, old_size, now],
            )?;

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

            Self::doc_sync_fts(&tx, rowid, content)?;
            tx.commit()?;
            drop(conn);
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

            // Insert + FTS sync share this transaction so a failure between
            // the two can't leave a document without its search index row.
            tx.execute(
                "INSERT INTO store_documents
                 (id, project_id, path, content, title, doc_type, blob_hash, mime, tags, session_id, source, metadata, size, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 1, ?14, ?14)",
                params![
                    id, project_id, path, content, title, doc_type, opts.blob_hash,
                    mime, tags_json, opts.session_id, source, metadata, size, now
                ],
            )?;
            let rowid = tx.last_insert_rowid();
            Self::doc_sync_fts(&tx, rowid, content)?;
            tx.commit()?;
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
                 FROM store_documents WHERE id = ?1",
            params![id],
            Self::row_to_document,
        )
        .optional()
    }

    pub fn doc_delete(&self, id: &str) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let now = db_kit::ids::now();
        if let Some(rowid) = conn
            .query_row(
                "SELECT rowid FROM store_documents WHERE id = ?1",
                params![id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            conn.execute(
                "UPDATE store_documents SET deleted_at = ?1 WHERE id = ?2",
                params![now, id],
            )?;
            conn.execute(
                "DELETE FROM store_docs_fts WHERE rowid = ?1",
                params![rowid],
            )?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn doc_hard_delete(&self, id: &str) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        let Some(rowid) = tx
            .query_row(
                "SELECT rowid FROM store_documents WHERE id = ?1",
                params![id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        else {
            return Ok(false);
        };

        // Delete dependents before the parent row, all in one transaction.
        tx.execute(
            "DELETE FROM store_doc_history WHERE doc_id = ?1",
            params![id],
        )?;
        tx.execute("DELETE FROM store_docs_vec WHERE doc_id = ?1", params![id])?;
        tx.execute(
            "DELETE FROM store_docs_fts WHERE rowid = ?1",
            params![rowid],
        )?;
        tx.execute("DELETE FROM store_documents WHERE id = ?1", params![id])?;
        tx.commit()?;
        Ok(true)
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

    pub fn doc_get_version(
        &self,
        doc_id: &str,
        version: i32,
    ) -> rusqlite::Result<Option<DocVersion>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT id, doc_id, version, content, blob_hash, mime, title, metadata, size, created_at
                 FROM store_doc_history WHERE doc_id = ?1 AND version = ?2",
            params![doc_id, version],
            |row| {
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
            },
        )
        .optional()
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
        let rows = stmt.query_map(params![query, project_id, limit as i64], |row| {
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
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
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
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_memory().unwrap()
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
