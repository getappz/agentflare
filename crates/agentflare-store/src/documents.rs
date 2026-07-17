use crate::Store;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Document {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub content: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DocMatch {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub snippet: String,
    pub score: f64,
}

impl Store {
    fn doc_sync_fts(&self, row_id: i64, content: &str) -> rusqlite::Result<()> {
        // FTS5 has no REPLACE/UPSERT — delete any existing rowid first (no-op if fresh)
        self.conn.execute(
            "DELETE FROM store_docs_fts WHERE rowid = ?1",
            params![row_id],
        )?;
        self.conn.execute(
            "INSERT INTO store_docs_fts(rowid, content) VALUES (?1, ?2)",
            params![row_id, content],
        )?;
        Ok(())
    }
    pub fn doc_upsert(
        &self,
        project_id: &str,
        path: &str,
        content: &str,
    ) -> rusqlite::Result<Document> {
        let now = db_kit::ids::now();
        let id = db_kit::ids::new_id();

        // Try to find existing by (project_id, path), else insert fresh
        let existing = self
            .conn
            .query_row(
                "SELECT id, rowid FROM store_documents WHERE project_id = ?1 AND path = ?2",
                params![project_id, path],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            )
            .optional()?;

        if let Some((existing_id, rowid)) = existing {
            self.conn.execute(
                "UPDATE store_documents SET content = ?1, updated_at = ?2, deleted_at = NULL WHERE id = ?3",
                params![content, now, existing_id],
            )?;
            self.doc_sync_fts(rowid, content)?;
            self.doc_get(&existing_id).map(|o| o.unwrap())
        } else {
            self.conn.execute(
                "INSERT INTO store_documents (id, project_id, path, content, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![id, project_id, path, content, now],
            )?;
            let rowid = self.conn.last_insert_rowid();
            self.doc_sync_fts(rowid, content)?;
            Ok(Document {
                id,
                project_id: project_id.to_string(),
                path: path.to_string(),
                content: content.to_string(),
                created_at: now,
                updated_at: now,
                deleted_at: None,
            })
        }
    }

    pub fn doc_get(&self, id: &str) -> rusqlite::Result<Option<Document>> {
        self.conn
            .query_row(
                "SELECT id, project_id, path, content, created_at, updated_at, deleted_at
                 FROM store_documents WHERE id = ?1",
                params![id],
                |row| {
                    Ok(Document {
                        id: row.get(0)?,
                        project_id: row.get(1)?,
                        path: row.get(2)?,
                        content: row.get(3)?,
                        created_at: row.get(4)?,
                        updated_at: row.get(5)?,
                        deleted_at: row.get(6)?,
                    })
                },
            )
            .optional()
    }

    pub fn doc_delete(&self, id: &str) -> rusqlite::Result<bool> {
        let now = db_kit::ids::now();
        if let Some(rowid) = self
            .conn
            .query_row(
                "SELECT rowid FROM store_documents WHERE id = ?1",
                params![id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            self.conn.execute(
                "UPDATE store_documents SET deleted_at = ?1 WHERE id = ?2",
                params![now, id],
            )?;
            self.conn.execute(
                "DELETE FROM store_docs_fts WHERE rowid = ?1",
                params![rowid],
            )?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn doc_hard_delete(&self, id: &str) -> rusqlite::Result<bool> {
        if let Some(rowid) = self
            .conn
            .query_row(
                "SELECT rowid FROM store_documents WHERE id = ?1",
                params![id],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
        {
            self.conn
                .execute("DELETE FROM store_documents WHERE id = ?1", params![id])?;
            self.conn.execute(
                "DELETE FROM store_docs_fts WHERE rowid = ?1",
                params![rowid],
            )?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn doc_search(
        &self,
        project_id: &str,
        query: &str,
        limit: usize,
    ) -> rusqlite::Result<Vec<DocMatch>> {
        let mut stmt = self.conn.prepare(
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
        let now = db_kit::ids::now();
        let bytes: &[u8] = bytemuck::cast_slice(embedding);
        let n = self.conn.execute(
            "INSERT INTO store_docs_vec (doc_id, embedding, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(doc_id) DO UPDATE SET embedding = ?2, updated_at = ?3",
            params![doc_id, bytes, now],
        )?;
        Ok(n > 0)
    }

    pub fn doc_get_embedding(&self, doc_id: &str) -> rusqlite::Result<Option<Vec<f32>>> {
        self.conn
            .query_row(
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
        let mut stmt = self.conn.prepare(
            "SELECT d.id, d.project_id, d.path, v.embedding
             FROM store_docs_vec v
             JOIN store_documents d ON d.id = v.doc_id
             WHERE d.project_id = ?1 AND d.deleted_at IS NULL",
        )?;
        let mut results: Vec<(f64, DocMatch)> = stmt
            .query_map(params![project_id], |row| {
                let id: String = row.get(0)?;
                let project_id: String = row.get(1)?;
                let path: String = row.get(2)?;
                let blob: Vec<u8> = row.get(3)?;
                Ok((id, project_id, path, blob))
            })?
            .filter_map(|r| r.ok())
            .filter_map(|(id, pid, path, blob)| {
                if blob.len() % 4 != 0 {
                    return None;
                }
                let doc_vec: Vec<f32> = blob
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
                let sim = crate::embed::cosine_similarity(query_vec, &doc_vec) as f64;
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
        let mut stmt = self.conn.prepare(
            "SELECT id, project_id, path, content, created_at, updated_at, deleted_at
             FROM store_documents
             WHERE project_id = ?1 AND deleted_at IS NULL
             ORDER BY path",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok(Document {
                id: row.get(0)?,
                project_id: row.get(1)?,
                path: row.get(2)?,
                content: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
                deleted_at: row.get(6)?,
            })
        })?;
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
    fn vec_search_ranks_by_similarity() {
        let s = store();
        let d1 = s.doc_upsert("p", "/cat.md", "about cats").unwrap();
        let d2 = s.doc_upsert("p", "/dog.md", "about dogs").unwrap();
        let d3 = s.doc_upsert("p", "/car.md", "about cars").unwrap();

        test_embed(&s, &d1.id, 4, 1.0);
        test_embed(&s, &d2.id, 4, 0.8);
        test_embed(&s, &d3.id, 4, 0.0);

        let query = vec![1.0; 4];
        let results = s.doc_vec_search("p", &query, 10).unwrap();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].id, d1.id);
        assert_eq!(results[1].id, d2.id);
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
}
