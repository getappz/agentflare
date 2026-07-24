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
