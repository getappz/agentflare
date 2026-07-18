use crate::Store;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct BlobMeta {
    pub hash: String,
    pub size: i64,
    pub ref_count: i32,
    pub created_at: i64,
}

const CHUNK_SIZE: usize = 64 * 1024;

fn blob_disk_path(root: &Path, hash: &str) -> PathBuf {
    root.join("blobs").join(&hash[..2]).join(hash)
}

/// `Ok(None)` means the file genuinely isn't there; other I/O errors (permissions,
/// disk failures) are propagated instead of being folded into "not found".
fn read_disk_blob(root: &Path, hash: &str) -> std::io::Result<Option<Vec<u8>>> {
    let path = blob_disk_path(root, hash);
    match std::fs::read(&path) {
        Ok(data) => Ok(Some(data)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn write_disk_blob(root: &Path, hash: &str, data: &[u8]) -> Result<(), std::io::Error> {
    let path = blob_disk_path(root, hash);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, data)?;
    Ok(())
}

fn delete_disk_blob(root: &Path, hash: &str) {
    let path = blob_disk_path(root, hash);
    let _ = std::fs::remove_file(&path);
}

use std::path::PathBuf;

impl Store {
    fn is_memory(&self) -> bool {
        self.root.to_string_lossy() == ":memory:"
    }

    pub fn blob_store(&self, data: &[u8]) -> rusqlite::Result<String> {
        let conn = self.conn();
        let hash = blake3::hash(data).to_hex().to_string();
        let now = db_kit::ids::now();
        let is_memory = self.is_memory();

        // Immediate takes the write lock up front, so the exists-check and
        // the insert-or-bump below are atomic across connections — without
        // this, two concurrent stores of the same new content can both see
        // "not found" and then race on the INSERT.
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;

        let exists = tx
            .query_row(
                "SELECT 1 FROM store_blobs WHERE hash = ?1",
                params![hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some();

        if exists {
            tx.execute(
                "UPDATE store_blobs SET ref_count = ref_count + 1 WHERE hash = ?1",
                params![hash],
            )?;
            tx.commit()?;
            return Ok(hash);
        }

        if !is_memory {
            // Written outside the SQL transaction (files aren't part of it);
            // if the metadata insert below fails, remove it again so a
            // failed store doesn't leak an orphaned file with no DB row.
            if let Err(e) = write_disk_blob(&self.root, &hash, data) {
                return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(e)));
            }
        } else {
            for (i, chunk) in data.chunks(CHUNK_SIZE).enumerate() {
                tx.execute(
                    "INSERT INTO store_blob_chunks (hash, chunk_index, data) VALUES (?1, ?2, ?3)",
                    params![hash, i as i64, chunk],
                )?;
            }
        }

        if let Err(e) = tx.execute(
            "INSERT INTO store_blobs (hash, size, ref_count, created_at) VALUES (?1, ?2, 1, ?3)",
            params![hash, data.len() as i64, now],
        ) {
            if !is_memory {
                delete_disk_blob(&self.root, &hash);
            }
            return Err(e);
        }
        tx.commit()?;
        Ok(hash)
    }

    pub fn blob_get(&self, hash: &str) -> rusqlite::Result<Option<Vec<u8>>> {
        let meta: BlobMeta = match self
            .conn
            .lock()
            .query_row(
                "SELECT hash, size, ref_count, created_at FROM store_blobs WHERE hash = ?1",
                params![hash],
                |row| {
                    Ok(BlobMeta {
                        hash: row.get(0)?,
                        size: row.get(1)?,
                        ref_count: row.get(2)?,
                        created_at: row.get(3)?,
                    })
                },
            )
            .optional()?
        {
            Some(m) => m,
            None => return Ok(None),
        };

        if !self.is_memory() {
            return read_disk_blob(&self.root, hash)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)));
        }

        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT data FROM store_blob_chunks WHERE hash = ?1 ORDER BY chunk_index")?;
        let chunks: rusqlite::Result<Vec<Vec<u8>>> =
            stmt.query_map(params![hash], |row| row.get(0))?.collect();

        let mut buf = Vec::with_capacity(meta.size as usize);
        for chunk in chunks? {
            buf.extend_from_slice(&chunk);
        }
        Ok(Some(buf))
    }

    pub fn blob_ref(&self, hash: &str) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let n = conn.execute(
            "UPDATE store_blobs SET ref_count = ref_count + 1 WHERE hash = ?1",
            params![hash],
        )?;
        Ok(n > 0)
    }

    pub fn blob_unref(&self, hash: &str) -> rusqlite::Result<bool> {
        let conn = self.conn();
        let is_memory = self.is_memory();

        // Immediate takes the write lock up front, so the decrement and the
        // ref_count<=0 cascade-delete below are atomic across connections —
        // without this, two concurrent unrefs can both observe ref_count<=0
        // and both attempt the cascade.
        let tx =
            rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;

        let n = tx.execute(
            "UPDATE store_blobs SET ref_count = ref_count - 1 WHERE hash = ?1 AND ref_count > 0",
            params![hash],
        )?;
        let mut removed = false;
        if n > 0 {
            removed = tx
                .query_row(
                    "SELECT ref_count <= 0 FROM store_blobs WHERE hash = ?1",
                    params![hash],
                    |row| row.get::<_, bool>(0),
                )
                .optional()?
                .unwrap_or(false);
            if removed {
                if is_memory {
                    tx.execute(
                        "DELETE FROM store_blob_chunks WHERE hash = ?1",
                        params![hash],
                    )?;
                }
                tx.execute("DELETE FROM store_blobs WHERE hash = ?1", params![hash])?;
            }
        }
        tx.commit()?;

        // Disk cleanup runs after the metadata commit: the row is the
        // source of truth and is already gone, so a crash here just leaks
        // a file instead of leaving a dangling row with no data.
        if removed && !is_memory {
            delete_disk_blob(&self.root, hash);
        }
        Ok(n > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_memory().unwrap()
    }

    #[test]
    fn store_and_retrieve() {
        let s = store();
        let data = b"hello blob store";
        let hash = s.blob_store(data).unwrap();
        assert_eq!(hash.len(), 64);

        let retrieved = s.blob_get(&hash).unwrap().unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn get_missing() {
        let s = store();
        assert!(s.blob_get("0000").unwrap().is_none());
    }

    #[test]
    fn dedup_same_content() {
        let s = store();
        let h1 = s.blob_store(b"same").unwrap();
        let h2 = s.blob_store(b"same").unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn ref_unref() {
        let s = store();
        let h = s.blob_store(b"data").unwrap();
        assert!(s.blob_ref(&h).unwrap());
        assert!(s.blob_unref(&h).unwrap());
        assert!(s.blob_unref(&h).unwrap());
        assert!(s.blob_get(&h).unwrap().is_none());
    }

    #[test]
    fn disk_storage() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("store.db");
        let s = Store::open_file(&db_path).unwrap();
        let data = b"content-addressed on disk";
        let hash = s.blob_store(data).unwrap();

        let disk_path = blob_disk_path(dir.path(), &hash);
        assert!(disk_path.exists(), "blob file should exist on disk");

        let retrieved = s.blob_get(&hash).unwrap().unwrap();
        assert_eq!(retrieved, data);
    }
}
