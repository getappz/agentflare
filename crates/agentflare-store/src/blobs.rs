use crate::Store;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct BlobMeta {
    pub hash: String,
    pub size: i64,
    pub ref_count: i32,
    pub created_at: i64,
}

const CHUNK_SIZE: usize = 64 * 1024; // 64 KiB

impl Store {
    pub fn blob_store(&self, data: &[u8]) -> rusqlite::Result<String> {
        let hash = blake3::hash(data).to_hex().to_string();
        let now = db_kit::ids::now();

        // Bump ref count if exists
        let exists = self
            .conn
            .query_row(
                "SELECT 1 FROM store_blobs WHERE hash = ?1",
                params![hash],
                |_| Ok(()),
            )
            .optional()?
            .is_some();

        if exists {
            self.conn.execute(
                "UPDATE store_blobs SET ref_count = ref_count + 1 WHERE hash = ?1",
                params![hash],
            )?;
            return Ok(hash);
        }

        self.conn.execute(
            "INSERT INTO store_blobs (hash, size, ref_count, created_at) VALUES (?1, ?2, 1, ?3)",
            params![hash, data.len() as i64, now],
        )?;

        for (i, chunk) in data.chunks(CHUNK_SIZE).enumerate() {
            self.conn.execute(
                "INSERT INTO store_blob_chunks (hash, chunk_index, data) VALUES (?1, ?2, ?3)",
                params![hash, i as i64, chunk],
            )?;
        }

        Ok(hash)
    }

    pub fn blob_get(&self, hash: &str) -> rusqlite::Result<Option<Vec<u8>>> {
        let meta: BlobMeta = match self
            .conn
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

        let mut stmt = self
            .conn
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
        let n = self.conn.execute(
            "UPDATE store_blobs SET ref_count = ref_count + 1 WHERE hash = ?1",
            params![hash],
        )?;
        Ok(n > 0)
    }

    pub fn blob_unref(&self, hash: &str) -> rusqlite::Result<bool> {
        let n = self.conn.execute(
            "UPDATE store_blobs SET ref_count = ref_count - 1 WHERE hash = ?1 AND ref_count > 0",
            params![hash],
        )?;
        if n > 0 {
            self.conn.execute(
                "DELETE FROM store_blobs WHERE hash = ?1 AND ref_count <= 0",
                params![hash],
            )?;
            self.conn.execute(
                "DELETE FROM store_blob_chunks WHERE hash = ?1",
                params![hash],
            )?;
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
}
