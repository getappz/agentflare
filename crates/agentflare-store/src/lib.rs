pub mod blobs;
pub mod documents;
pub mod embed;
pub mod kv;
pub mod leases;
pub mod maintenance;
pub mod migrate;
pub mod migrations;
pub mod retrieval;

#[cfg(feature = "embeddings")]
pub mod embedding_pipeline;

use rusqlite::Connection;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Migration(#[from] rusqlite_migration::Error),
    #[error(transparent)]
    DbKit(#[from] db_kit::open::Error),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("blob too large: {size} > {max}")]
    BlobTooLarge { size: u64, max: u64 },
    #[error("lease denied: {0}")]
    LeaseDenied(String),
}

pub struct Store {
    conn: parking_lot::Mutex<Connection>,
    root: PathBuf,
    /// Where this store writes blob files. Namespaced per db file because
    /// `store_blobs` ref counts are per db: two stores sharing a directory
    /// (`~/.agentflare/store.db` and `flare-docs.db`) can't see each other's
    /// references, so a shared blob dir lets one store's GC delete content the
    /// other still points at.
    blob_dir: PathBuf,
}

impl Store {
    pub fn open_file(path: &Path) -> Result<Self, Error> {
        let conn = db_kit::open_file(path, &migrations::migrations())?;
        let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let blob_dir = root.join(format!("blobs-{stem}"));
        Ok(Self {
            conn: parking_lot::Mutex::new(conn),
            root,
            blob_dir,
        })
    }

    pub fn open_memory() -> Result<Self, Error> {
        let conn = db_kit::open_memory(&migrations::migrations())?;
        Ok(Self {
            conn: parking_lot::Mutex::new(conn),
            root: PathBuf::from(":memory:"),
            blob_dir: PathBuf::from(":memory:"),
        })
    }

    pub fn conn(&self) -> parking_lot::MutexGuard<'_, Connection> {
        self.conn.lock()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_memory_store() {
        let store = Store::open_memory().unwrap();
        store.conn().execute_batch("SELECT 1").unwrap();
    }

    #[test]
    fn open_file_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("store.db");
        let store = Store::open_file(&path).unwrap();
        assert!(store.root().exists());
    }
}
