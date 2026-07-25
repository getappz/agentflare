use agentflare_store::documents::{DocMatch, DocUpsertOpts, Document};
use std::path::{Path, PathBuf};

/// Every row in the flare-docs store uses this fixed project_id. agentflare-store's
/// doc_* methods require project_id as a mandatory param; using one constant value
/// across every call is what makes this store logically global (fetched once,
/// reused by every project) rather than scoped per-project.
pub const PROJECT_ID: &str = "global";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Store(#[from] agentflare_store::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

pub struct DocsStore {
    inner: agentflare_store::Store,
}

impl DocsStore {
    pub fn open_memory() -> Result<Self, Error> {
        Ok(Self {
            inner: agentflare_store::Store::open_memory()?,
        })
    }

    pub fn open_file(path: &Path) -> Result<Self, Error> {
        Ok(Self {
            inner: agentflare_store::Store::open_file(path)?,
        })
    }

    pub fn default_db_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".agentflare")
            .join("flare-docs.db")
    }

    pub fn open_default() -> Result<Self, Error> {
        let path = Self::default_db_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self::open_file(&path)
    }

    pub fn upsert(
        &self,
        id_path: &str,
        content: &str,
        opts: DocUpsertOpts,
    ) -> Result<Document, Error> {
        Ok(self
            .inner
            .doc_upsert_with_opts(PROJECT_ID, id_path, content, opts)?)
    }

    pub fn get(&self, id: &str) -> Result<Option<Document>, Error> {
        Ok(self.inner.doc_get(id)?)
    }

    pub fn get_by_path(&self, path: &str) -> Result<Option<Document>, Error> {
        Ok(self.inner.doc_get_by_path(PROJECT_ID, path)?)
    }

    pub fn search(&self, query: &str, limit: usize) -> Result<Vec<DocMatch>, Error> {
        Ok(self.inner.doc_search(PROJECT_ID, query, limit)?)
    }

    pub fn list(&self) -> Result<Vec<Document>, Error> {
        Ok(self.inner.doc_list(PROJECT_ID)?)
    }

    pub fn blob_store_raw(&self, data: &[u8]) -> Result<String, Error> {
        Ok(self.inner.blob_store(data)?)
    }

    /// Upserts many documents in a single transaction.
    ///
    /// A crate's rustdoc JSON can have thousands of items; upserting them
    /// one at a time through [`Self::upsert`] would open a separate
    /// `BEGIN IMMEDIATE` transaction (and FTS resync) per item, turning a
    /// single `get`/`refresh` into thousands of exclusive-lock commits.
    /// This batches the whole set into one transaction and never writes
    /// `store_doc_history` rows — cache-type per-item docs (rustdoc text is
    /// stable across refreshes) have no use for version history, and
    /// writing it unconditionally would be pure amplification at this
    /// volume.
    pub fn upsert_batch(&self, items: &[BatchItem]) -> Result<usize, Error> {
        let conn = self.inner.conn();
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
        let now = db_kit::ids::now();

        for item in items {
            let tags_json = serde_json::to_string(&item.tags).unwrap_or_else(|_| "[]".into());
            let id = db_kit::ids::new_id();
            let rowid: i64 = tx.query_row(
                "INSERT INTO store_documents
                 (id, project_id, path, content, title, doc_type, blob_hash, mime, tags, session_id, source, metadata, size, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, '', ?7, NULL, ?8, '{}', 0, 1, ?9, ?9)
                 ON CONFLICT(project_id, path) DO UPDATE SET
                    content = excluded.content,
                    title = excluded.title,
                    doc_type = excluded.doc_type,
                    tags = excluded.tags,
                    source = excluded.source,
                    version = store_documents.version + 1,
                    updated_at = excluded.updated_at,
                    deleted_at = NULL
                 RETURNING rowid",
                rusqlite::params![
                    id,
                    PROJECT_ID,
                    item.path,
                    item.content,
                    item.title,
                    item.doc_type,
                    tags_json,
                    item.source,
                    now,
                ],
                |row| row.get(0),
            )?;

            tx.execute(
                "DELETE FROM store_docs_fts WHERE rowid = ?1",
                rusqlite::params![rowid],
            )?;
            tx.execute(
                "INSERT INTO store_docs_fts(rowid, content) VALUES (?1, ?2)",
                rusqlite::params![rowid, item.content],
            )?;
        }

        tx.commit()?;
        Ok(items.len())
    }
}

/// One document to write via [`DocsStore::upsert_batch`].
#[derive(Debug, Clone)]
pub struct BatchItem {
    pub path: String,
    pub content: String,
    pub title: String,
    pub doc_type: String,
    pub tags: Vec<String>,
    pub source: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_get_and_search_round_trip() {
        let store = DocsStore::open_memory().unwrap();

        let opts = DocUpsertOpts {
            title: Some("serde".to_string()),
            doc_type: Some("rust-crate".to_string()),
            source: Some("docsrs".to_string()),
            tags: Some(vec!["serde".to_string(), "rust".to_string()]),
            ..Default::default()
        };
        let doc = store
            .upsert(
                "docsrs/serde",
                "A generic serialization/deserialization framework",
                opts,
            )
            .unwrap();
        assert_eq!(doc.project_id, PROJECT_ID);
        assert_eq!(doc.path, "docsrs/serde");
        assert_eq!(doc.doc_type, "rust-crate");
        assert_eq!(doc.tags, vec!["serde", "rust"]);

        let fetched = store.get(&doc.id).unwrap().unwrap();
        assert_eq!(
            fetched.content,
            "A generic serialization/deserialization framework"
        );

        let hits = store.search("serialization", 10).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, doc.id);

        let listed = store.list().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, doc.id);
    }

    #[test]
    fn upsert_batch_inserts_multiple_documents_in_one_call() {
        let store = DocsStore::open_memory().unwrap();
        let items = vec![
            BatchItem {
                path: "docsrs/axum/latest/axum::extract::State".into(),
                content: "State docs".into(),
                title: "State".into(),
                doc_type: "rust-item".into(),
                tags: vec!["axum".into(), "rust".into(), "struct".into()],
                source: "docsrs".into(),
            },
            BatchItem {
                path: "docsrs/axum/latest/axum::Router".into(),
                content: "Router docs".into(),
                title: "Router".into(),
                doc_type: "rust-item".into(),
                tags: vec!["axum".into(), "rust".into(), "struct".into()],
                source: "docsrs".into(),
            },
        ];

        let n = store.upsert_batch(&items).unwrap();
        assert_eq!(n, 2);

        let state_doc = store
            .get_by_path("docsrs/axum/latest/axum::extract::State")
            .unwrap()
            .unwrap();
        assert_eq!(state_doc.content, "State docs");
        assert_eq!(state_doc.title, "State");
        assert_eq!(state_doc.project_id, PROJECT_ID);

        let hits = store.search("State docs", 10).unwrap();
        assert_eq!(hits.len(), 1);
    }

    #[test]
    fn upsert_batch_updates_existing_row_and_preserves_id() {
        let store = DocsStore::open_memory().unwrap();
        let make = |content: &str| {
            vec![BatchItem {
                path: "docsrs/foo/latest/foo::Bar".into(),
                content: content.into(),
                title: "Bar".into(),
                doc_type: "rust-item".into(),
                tags: vec![],
                source: "docsrs".into(),
            }]
        };

        store.upsert_batch(&make("v1")).unwrap();
        let original = store
            .get_by_path("docsrs/foo/latest/foo::Bar")
            .unwrap()
            .unwrap();

        store.upsert_batch(&make("v2")).unwrap();
        let updated = store
            .get_by_path("docsrs/foo/latest/foo::Bar")
            .unwrap()
            .unwrap();

        assert_eq!(updated.id, original.id);
        assert_eq!(updated.content, "v2");
        assert_eq!(updated.version, 2);
    }

    #[test]
    fn get_by_path_finds_an_existing_doc_and_none_for_a_missing_one() {
        let store = DocsStore::open_memory().unwrap();
        store
            .upsert("docsrs/serde", "docs", DocUpsertOpts::default())
            .unwrap();

        let found = store.get_by_path("docsrs/serde").unwrap().unwrap();
        assert_eq!(found.path, "docsrs/serde");

        assert!(store.get_by_path("docsrs/nope").unwrap().is_none());
    }
}
