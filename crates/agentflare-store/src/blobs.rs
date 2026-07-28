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

/// `dir` is a blob directory (`Store::blob_dir` or the legacy shared one), not
/// the store root.
fn blob_disk_path(dir: &Path, hash: &str) -> PathBuf {
    dir.join(&hash[..2]).join(hash)
}

/// Reads the store's own blob directory first, then the legacy shared
/// `<root>/blobs` where builds before per-store namespacing wrote everything.
///
/// `Ok(None)` means the file genuinely isn't there; other I/O errors (permissions,
/// disk failures) are propagated instead of being folded into "not found".
fn read_disk_blob(dir: &Path, legacy_dir: &Path, hash: &str) -> std::io::Result<Option<Vec<u8>>> {
    for d in [dir, legacy_dir] {
        match std::fs::read(blob_disk_path(d, hash)) {
            Ok(data) => return Ok(Some(decompress_if_gzip(data)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

fn write_disk_blob(dir: &Path, hash: &str, data: &[u8]) -> Result<(), std::io::Error> {
    let path = blob_disk_path(dir, hash);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, gzip(data)?)?;
    Ok(())
}

fn gzip(data: &[u8]) -> std::io::Result<Vec<u8>> {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::io::Write;
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data)?;
    enc.finish()
}

/// Gzip streams always start with the 2-byte magic `1f 8b`; a blob written
/// before compression landed has no such header, so it's returned
/// byte-for-byte unchanged — self-describing on read, no version marker or
/// migration step needed to keep serving blobs written by older builds.
fn decompress_if_gzip(data: Vec<u8>) -> std::io::Result<Vec<u8>> {
    if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
        use flate2::read::GzDecoder;
        use std::io::Read;
        let mut out = Vec::new();
        GzDecoder::new(&data[..]).read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(data)
    }
}

/// Only ever unlinks from this store's own blob directory. The legacy shared
/// `<root>/blobs` may hold bytes another store's ref counts still cover, and
/// this store can't see that table — leaking a legacy file is recoverable,
/// deleting one another store needs is not.
fn delete_disk_blob(dir: &Path, hash: &str) {
    let path = blob_disk_path(dir, hash);
    // The row is already gone by the time this runs (see blob_unref), so a
    // failure here can't be retried through the database — log it (unless the
    // file was simply already absent) so an orphaned file is at least
    // discoverable instead of silently unaccounted for.
    if let Err(e) = std::fs::remove_file(&path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        eprintln!(
            "[store] failed to reclaim blob file {}: {e}",
            path.display()
        );
    }
}

use std::path::PathBuf;

impl Store {
    fn is_memory(&self) -> bool {
        self.root.to_string_lossy() == ":memory:"
    }

    fn legacy_blob_dir(&self) -> PathBuf {
        self.root.join("blobs")
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
            if let Err(e) = write_disk_blob(&self.blob_dir, &hash, data) {
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
                delete_disk_blob(&self.blob_dir, &hash);
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
            return read_disk_blob(&self.blob_dir, &self.legacy_blob_dir(), hash)
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
            delete_disk_blob(&self.blob_dir, hash);
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

        let disk_path = blob_disk_path(&s.blob_dir, &hash);
        assert!(disk_path.exists(), "blob file should exist on disk");

        let retrieved = s.blob_get(&hash).unwrap().unwrap();
        assert_eq!(retrieved, data);
    }

    #[test]
    fn disk_blobs_are_stored_gzip_compressed() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("store.db");
        let s = Store::open_file(&db_path).unwrap();
        // Long enough / repetitive enough that gzip overhead can't win.
        let data = "compressible ".repeat(200);
        let hash = s.blob_store(data.as_bytes()).unwrap();

        let on_disk = std::fs::read(blob_disk_path(&s.blob_dir, &hash)).unwrap();
        assert!(
            on_disk.len() < data.len(),
            "on-disk blob ({} bytes) should be smaller than the source ({} bytes)",
            on_disk.len(),
            data.len()
        );
        assert_eq!(&on_disk[..2], &[0x1f, 0x8b], "gzip magic header");

        let retrieved = s.blob_get(&hash).unwrap().unwrap();
        assert_eq!(retrieved, data.as_bytes());
    }

    #[test]
    fn a_legacy_uncompressed_blob_written_before_gzip_still_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("store.db");
        let s = Store::open_file(&db_path).unwrap();
        let data = b"plain bytes, no gzip header";
        let hash = blake3::hash(data).to_hex().to_string();

        // Simulate a blob written by a build predating compression: write
        // the raw disk file directly (bypassing blob_store's gzip step) and
        // register its DB row the same way blob_store would.
        let disk_path = blob_disk_path(&s.blob_dir, &hash);
        std::fs::create_dir_all(disk_path.parent().unwrap()).unwrap();
        std::fs::write(&disk_path, data).unwrap();
        s.conn.lock().execute(
            "INSERT INTO store_blobs (hash, size, ref_count, created_at) VALUES (?1, ?2, 1, ?3)",
            params![hash, data.len() as i64, db_kit::ids::now()],
        ).unwrap();

        let retrieved = s.blob_get(&hash).unwrap().unwrap();
        assert_eq!(
            retrieved, data,
            "a pre-compression blob has no gzip magic and must pass through unchanged"
        );
    }

    fn blob_backed_doc(s: &Store, path: &str, hash: &str) -> crate::documents::Document {
        s.doc_upsert_with_opts(
            "p",
            path,
            "",
            crate::documents::DocUpsertOpts {
                blob_hash: Some(hash.to_string()),
                ..Default::default()
            },
        )
        .unwrap()
    }

    // File-backed on purpose: only `open_memory` stores blob bytes in
    // store_blob_chunks, so an in-memory version of this test would pass
    // while the real on-disk path kept leaking.
    #[test]
    fn doc_delete_removes_the_disk_blob_of_its_last_referrer() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_file(&dir.path().join("store.db")).unwrap();
        let hash = s.blob_store(b"rustdoc json payload").unwrap();
        let doc = blob_backed_doc(&s, "/docsrs/serde/latest", &hash);

        let disk_path = blob_disk_path(&s.blob_dir, &hash);
        assert!(disk_path.exists(), "precondition: blob written to disk");

        s.doc_delete(&doc.id).unwrap();

        assert!(
            !disk_path.exists(),
            "deleting the only document referencing a blob must reclaim its disk file"
        );
        assert!(s.blob_get(&hash).unwrap().is_none());
    }

    fn cached_blob_doc(s: &Store, path: &str, hash: &str) {
        s.doc_upsert_with_opts(
            "p",
            path,
            "",
            crate::documents::DocUpsertOpts {
                blob_hash: Some(hash.to_string()),
                // Cache-type caller: no history snapshot, so a replaced blob
                // is genuinely unreferenced afterwards.
                track_history: false,
                ..Default::default()
            },
        )
        .unwrap();
    }

    #[test]
    fn upserting_a_new_blob_over_an_old_one_reclaims_the_old_file() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_file(&dir.path().join("store.db")).unwrap();
        let old_hash = s.blob_store(b"version one payload").unwrap();
        cached_blob_doc(&s, "/cached", &old_hash);
        let old_path = blob_disk_path(&s.blob_dir, &old_hash);
        assert!(old_path.exists(), "precondition: v1 blob on disk");

        // Same path, new blob: the row stops pointing at the old hash, so its
        // last reference is gone even though no document was deleted.
        let new_hash = s.blob_store(b"version two payload").unwrap();
        cached_blob_doc(&s, "/cached", &new_hash);

        assert!(
            !old_path.exists(),
            "a superseded blob must be reclaimed, not left behind by the update"
        );
        assert!(
            blob_disk_path(&s.blob_dir, &new_hash).exists(),
            "the replacement blob must survive"
        );
        assert!(s.blob_get(&new_hash).unwrap().is_some());
    }

    #[test]
    fn upserting_a_new_blob_keeps_the_old_one_when_history_snapshots_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_file(&dir.path().join("store.db")).unwrap();
        let old_hash = s.blob_store(b"version one payload").unwrap();
        blob_backed_doc(&s, "/versioned", &old_hash);

        // Default opts track history, so the update snapshots old_hash into
        // store_doc_history -- reclaiming it would empty the previous version.
        let new_hash = s.blob_store(b"version two payload").unwrap();
        blob_backed_doc(&s, "/versioned", &new_hash);

        assert!(
            blob_disk_path(&s.blob_dir, &old_hash).exists(),
            "a blob a history row still points at must not be reclaimed"
        );
        assert_eq!(
            s.blob_get(&old_hash).unwrap().as_deref(),
            Some(&b"version one payload"[..]),
            "the previous version must still read back"
        );
    }

    #[test]
    fn doc_delete_keeps_a_blob_alive_while_another_document_still_references_it() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_file(&dir.path().join("store.db")).unwrap();
        // Same bytes twice: blob_store dedupes and bumps ref_count to 2.
        let hash = s.blob_store(b"shared payload").unwrap();
        assert_eq!(s.blob_store(b"shared payload").unwrap(), hash);
        let first = blob_backed_doc(&s, "/a", &hash);
        let second = blob_backed_doc(&s, "/b", &hash);

        let disk_path = blob_disk_path(&s.blob_dir, &hash);
        s.doc_delete(&first.id).unwrap();
        assert!(
            disk_path.exists(),
            "a blob still referenced by another document must survive"
        );

        // Re-deleting an already soft-deleted document must not unref twice —
        // that would strand `second`'s content while its row still looks live.
        s.doc_delete(&first.id).unwrap();
        assert!(disk_path.exists(), "double delete must not double-unref");

        s.doc_delete(&second.id).unwrap();
        assert!(
            !disk_path.exists(),
            "the last referrer's delete must reclaim the file"
        );
    }

    // ~/.agentflare/store.db and ~/.agentflare/flare-docs.db share a directory
    // but keep separate store_blobs tables, so neither can see the other's
    // references. Sharing one blob directory would let either one's reclaim
    // strand the other's content.
    #[test]
    fn a_reclaim_cannot_strand_identical_bytes_held_by_a_store_sharing_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let a = Store::open_file(&dir.path().join("store.db")).unwrap();
        let b = Store::open_file(&dir.path().join("flare-docs.db")).unwrap();

        let data = b"identical bytes stored in both";
        let ha = a.blob_store(data).unwrap();
        let hb = b.blob_store(data).unwrap();
        assert_eq!(ha, hb, "content addressing collides these by design");

        // a drops its only reference; b never unreffed.
        assert!(a.blob_unref(&ha).unwrap());
        assert!(a.blob_get(&ha).unwrap().is_none());

        assert_eq!(
            b.blob_get(&hb).unwrap().as_deref(),
            Some(&data[..]),
            "the neighbouring store still references these bytes"
        );
    }

    /// Seeds a blob the way a build predating per-store blob dirs would have:
    /// file in the shared `<root>/blobs`, row in this store's table.
    fn seed_legacy_blob(s: &Store, root: &Path, data: &[u8]) -> String {
        let hash = blake3::hash(data).to_hex().to_string();
        let path = blob_disk_path(&root.join("blobs"), &hash);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, gzip(data).unwrap()).unwrap();
        s.conn
            .lock()
            .execute(
                "INSERT INTO store_blobs (hash, size, ref_count, created_at) VALUES (?1, ?2, 1, ?3)",
                params![hash, data.len() as i64, db_kit::ids::now()],
            )
            .unwrap();
        hash
    }

    #[test]
    fn a_blob_in_the_legacy_shared_dir_still_reads_back() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_file(&dir.path().join("store.db")).unwrap();
        let data = b"stored before blob dirs were namespaced";
        let hash = seed_legacy_blob(&s, dir.path(), data);

        assert_eq!(
            s.blob_get(&hash).unwrap().as_deref(),
            Some(&data[..]),
            "reads must fall back to the legacy shared directory"
        );
    }

    #[test]
    fn reclaiming_a_blob_never_unlinks_from_the_legacy_shared_dir() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_file(&dir.path().join("store.db")).unwrap();
        let data = b"legacy bytes another store may still reference";
        let hash = seed_legacy_blob(&s, dir.path(), data);
        let legacy_path = blob_disk_path(&dir.path().join("blobs"), &hash);

        assert!(s.blob_unref(&hash).unwrap());

        assert!(
            legacy_path.exists(),
            "a leaked legacy file is recoverable; deleting one a neighbouring store needs is not"
        );
    }
}
