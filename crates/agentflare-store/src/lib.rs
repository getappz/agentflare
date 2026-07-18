pub mod blobs;
pub mod documents;
pub mod embed;
pub mod kv;
pub mod leases;
pub mod migrate;
pub mod migrations;

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
}

impl Store {
    pub fn open_file(path: &Path) -> Result<Self, Error> {
        let conn = db_kit::open_file(path, &migrations::migrations())?;
        let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        Ok(Self {
            conn: parking_lot::Mutex::new(conn),
            root,
        })
    }

    pub fn open_memory() -> Result<Self, Error> {
        let conn = db_kit::open_memory(&migrations::migrations())?;
        Ok(Self {
            conn: parking_lot::Mutex::new(conn),
            root: PathBuf::from(":memory:"),
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
