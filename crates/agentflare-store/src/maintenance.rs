//! Cache maintenance: purge expired tombstones, evict to a size budget,
//! reclaim the freed pages.
//!
//! Soft deletion (`deleted_at`) is what keeps a refetch from resurrecting an
//! item that disappeared upstream, but nothing ever removed those rows, and
//! nothing capped the store's growth at all — a long-lived install that
//! refreshes a rotating set of packages accumulates dead rows and live-but-
//! never-read ones indefinitely (#339).
//!
//! Three phases, in order, because each one's input depends on the last:
//! purge expired tombstones, then evict live documents if what remains is
//! still over budget, then hand the freed pages back to the filesystem.

use crate::Store;
use rusqlite::{OptionalExtension, params};
use serde::Serialize;

/// How long a soft-deleted row is kept before it is physically removed.
///
/// Not zero: `deleted_at` is also the record that an item *was* here, and a
/// package that drops an item in one release and restores it in the next
/// should not look like a brand-new document. A week covers that without
/// keeping dead rows around indefinitely.
pub const DEFAULT_RETENTION_SECS: i64 = 7 * 24 * 60 * 60;

/// Byte budget for one store's live content.
pub const DEFAULT_MAX_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct GcOpts {
    pub retention_secs: i64,
    pub max_bytes: u64,
}

impl Default for GcOpts {
    fn default() -> Self {
        Self {
            retention_secs: DEFAULT_RETENTION_SECS,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// What one [`Store::gc`] run did. Serializable because evictions are
/// journaled — a cache that silently drops content the caller believes is
/// there needs a record of what went and why.
#[derive(Debug, Default, Clone, Serialize)]
pub struct GcReport {
    /// Expired tombstones physically removed.
    pub purged: usize,
    /// Live documents dropped to fit `max_bytes`.
    pub evicted: usize,
    /// Blob references released across both phases.
    pub blobs_released: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// Whether the reclaim step had to run a full `VACUUM` (a one-time
    /// conversion of a pre-`auto_vacuum` database) rather than an
    /// incremental one.
    pub vacuumed: bool,
}

impl Store {
    /// Bytes of live content this project holds: inline document text plus
    /// the blobs it references.
    ///
    /// Deliberately not the size of the `.db` file. The blobs dominate for a
    /// file-backed store — measured 5.7 MB of blobs against a 1.8 MB
    /// database for a routine docs cache — so a cap that looked only at the
    /// file would never fire. The database file also does not shrink at the
    /// moment rows are deleted, which would make it useless as the loop
    /// variable for eviction: pages come back only at the reclaim step,
    /// after the decisions have already been made.
    ///
    /// Blob bytes are counted whole even though `store_blobs` is shared
    /// across projects within one database. For a single-project cache
    /// (flare-docs) that is exact; for a multi-project store it
    /// over-attributes, which errs toward collecting sooner.
    pub fn cache_bytes(&self, project_id: &str) -> rusqlite::Result<u64> {
        let conn = self.conn();
        let content: i64 = conn.query_row(
            "SELECT COALESCE(SUM(length(CAST(content AS BLOB))), 0) FROM store_documents
             WHERE project_id = ?1 AND deleted_at IS NULL",
            params![project_id],
            |row| row.get(0),
        )?;
        let blobs: i64 = conn.query_row(
            "SELECT COALESCE(SUM(size), 0) FROM store_blobs",
            [],
            |row| row.get(0),
        )?;
        Ok((content + blobs).max(0) as u64)
    }

    /// Purges expired tombstones, evicts live documents while the project is
    /// over `max_bytes`, and reclaims the freed pages.
    ///
    /// Runs its own transactions rather than taking one from the caller:
    /// blob release and `VACUUM` both need the connection to themselves.
    pub fn gc(&self, project_id: &str, opts: GcOpts) -> rusqlite::Result<GcReport> {
        let mut report = GcReport {
            bytes_before: self.cache_bytes(project_id)?,
            ..GcReport::default()
        };

        let cutoff = db_kit::ids::now() - opts.retention_secs;
        let expired: Vec<String> = {
            let conn = self.conn();
            let mut stmt = conn.prepare(
                "SELECT id FROM store_documents
                 WHERE project_id = ?1 AND deleted_at IS NOT NULL AND deleted_at < ?2",
            )?;
            stmt.query_map(params![project_id, cutoff], |row| row.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        report.purged = expired.len();
        // `release_own_blob: false` — a tombstone's own blob reference was
        // already dropped by whatever soft-deleted it (`doc_delete`,
        // `upsert_batch_reconciled`). Releasing it again here would take a
        // reference the row no longer holds, emptying a blob some live
        // document still points at.
        report.blobs_released += self.hard_delete_docs(&expired, false)?;

        for id in self.eviction_candidates(project_id, opts.max_bytes)? {
            report.blobs_released += self.hard_delete_docs(std::slice::from_ref(&id), true)?;
            report.evicted += 1;
        }

        if report.purged + report.evicted > 0 {
            report.vacuumed = self.reclaim()?;
        }
        report.bytes_after = self.cache_bytes(project_id)?;
        Ok(report)
    }

    /// Live documents to drop, oldest first, until the project fits its
    /// budget. Empty while under budget — the common case, one query.
    ///
    /// `updated_at` order, not true LRU: a real last-accessed column costs a
    /// write on every read, and for a cache that is refreshed rather than
    /// mutated the two orders differ very little. A blob counts toward the
    /// bytes its document frees only when nothing else references it.
    fn eviction_candidates(
        &self,
        project_id: &str,
        max_bytes: u64,
    ) -> rusqlite::Result<Vec<String>> {
        let over = self.cache_bytes(project_id)?.saturating_sub(max_bytes);
        if over == 0 {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT d.id,
                    length(CAST(d.content AS BLOB))
                    + COALESCE((SELECT b.size FROM store_blobs b
                                 WHERE b.hash = d.blob_hash AND b.ref_count = 1), 0)
             FROM store_documents d
             WHERE d.project_id = ?1 AND d.deleted_at IS NULL
             ORDER BY d.updated_at ASC, d.rowid ASC",
        )?;
        let rows = stmt.query_map(params![project_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;

        let mut freed: u64 = 0;
        let mut victims = Vec::new();
        for row in rows {
            let (id, bytes) = row?;
            victims.push(id);
            freed = freed.saturating_add(bytes.max(0) as u64);
            if freed >= over {
                break;
            }
        }
        Ok(victims)
    }

    /// Physically removes documents and everything hanging off them,
    /// returning the number of blob references released.
    ///
    /// History snapshots are deleted first and their blobs released: a
    /// snapshot is what keeps a superseded blob alive (it is how
    /// `doc_history` reads the old bytes back), so dropping the rows without
    /// the matching unref would orphan those files forever — the same leak
    /// this issue's first fix closed on the soft-delete path.
    ///
    /// `release_own_blob` distinguishes the two callers: an evicted document
    /// is live and still holds its own reference; an expired tombstone gave
    /// its up when it was soft-deleted.
    fn hard_delete_docs(&self, ids: &[String], release_own_blob: bool) -> rusqlite::Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        // Scoped so the guard is released before blob_unref, which takes the
        // same non-reentrant mutex and opens its own transaction.
        let hashes = {
            let conn = self.conn();
            let tx = rusqlite::Transaction::new_unchecked(
                &conn,
                rusqlite::TransactionBehavior::Immediate,
            )?;
            let mut hashes: Vec<String> = Vec::new();
            for id in ids {
                let mut stmt = tx.prepare(
                    "SELECT blob_hash FROM store_doc_history
                     WHERE doc_id = ?1 AND blob_hash IS NOT NULL",
                )?;
                let history: Vec<String> = stmt
                    .query_map(params![id], |row| row.get(0))?
                    .collect::<rusqlite::Result<_>>()?;
                drop(stmt);
                hashes.extend(history);

                if release_own_blob {
                    let own: Option<String> = tx
                        .query_row(
                            "SELECT blob_hash FROM store_documents WHERE id = ?1",
                            params![id],
                            |row| row.get(0),
                        )
                        .optional()?
                        .flatten();
                    hashes.extend(own);
                }

                // store_doc_history has a REFERENCES on store_documents, so
                // with foreign keys enforced this ordering is required, not
                // just tidy. store_docs_vec has no constraint and would
                // simply be orphaned.
                tx.execute(
                    "DELETE FROM store_doc_history WHERE doc_id = ?1",
                    params![id],
                )?;
                tx.execute("DELETE FROM store_docs_vec WHERE doc_id = ?1", params![id])?;
                // Dropping the row out of store_docs_fts is the AFTER DELETE
                // trigger's job — see migrations::EXTERNAL_CONTENT_FTS_MIGRATION.
                tx.execute("DELETE FROM store_documents WHERE id = ?1", params![id])?;
            }
            tx.commit()?;
            hashes
        };

        // After the commit, matching doc_delete: the rows are the source of
        // truth and are already gone, so a failure here leaks a file rather
        // than stranding a document with no content. Every hash gets its
        // attempt before the first error is reported — returning early would
        // strand the rest as orphans with nothing left to roll back.
        let mut released = 0;
        let mut first_err = None;
        for hash in hashes {
            match self.blob_unref(&hash) {
                Ok(true) => released += 1,
                Ok(false) => {}
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(released),
        }
    }

    /// Returns the pages freed by this run to the filesystem. `true` if a
    /// full `VACUUM` was needed.
    ///
    /// `auto_vacuum` only takes effect on a database that had it set before
    /// its first table existed; one created earlier stays in `NONE` mode
    /// until a single full `VACUUM` rewrites it, and `open_file` has already
    /// asked for `INCREMENTAL`, so that rewrite is what makes the setting
    /// stick. From then on the cheap incremental path runs instead.
    ///
    /// `VACUUM` may renumber the implicit rowids that the external-content
    /// FTS index is keyed on, so the index is rebuilt behind it — without
    /// that, search would return rows by stale rowid.
    fn reclaim(&self) -> rusqlite::Result<bool> {
        let full = {
            let conn = self.conn();
            let mode: i64 = conn.query_row("PRAGMA auto_vacuum", [], |row| row.get(0))?;
            if mode == 0 {
                conn.execute_batch("VACUUM")?;
                true
            } else {
                conn.execute_batch("PRAGMA incremental_vacuum")?;
                false
            }
        };
        if full {
            self.doc_fts_rebuild()?;
        }
        Ok(full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::DocUpsertOpts;

    const PROJECT: &str = "p";

    fn store() -> Store {
        Store::open_memory().unwrap()
    }

    fn backdate_deletion(store: &Store, id: &str, secs_ago: i64) {
        let when = db_kit::ids::now() - secs_ago;
        store
            .conn()
            .execute(
                "UPDATE store_documents SET deleted_at = ?1 WHERE id = ?2",
                params![when, id],
            )
            .unwrap();
    }

    fn rowids(store: &Store, table: &str) -> Vec<i64> {
        let conn = store.conn();
        let mut stmt = conn
            .prepare(&format!("SELECT rowid FROM {table} ORDER BY rowid"))
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn row_exists(store: &Store, id: &str) -> bool {
        store
            .conn()
            .query_row(
                "SELECT 1 FROM store_documents WHERE id = ?1",
                params![id],
                |_| Ok(()),
            )
            .optional()
            .unwrap()
            .is_some()
    }

    #[test]
    fn expired_tombstones_are_purged_and_fresh_ones_kept() {
        let s = store();
        let old = s.doc_upsert(PROJECT, "old.md", "gone upstream").unwrap();
        let recent = s.doc_upsert(PROJECT, "recent.md", "just removed").unwrap();
        s.doc_delete(&old.id).unwrap();
        s.doc_delete(&recent.id).unwrap();
        backdate_deletion(&s, &old.id, DEFAULT_RETENTION_SECS + 60);

        let report = s.gc(PROJECT, GcOpts::default()).unwrap();

        assert_eq!(report.purged, 1);
        assert!(!row_exists(&s, &old.id));
        assert!(row_exists(&s, &recent.id));
    }

    #[test]
    fn purging_a_document_releases_the_blobs_its_history_still_held() {
        let s = store();
        let old_blob = s.blob_store(b"version one payload").unwrap();
        let new_blob = s.blob_store(b"version two payload").unwrap();
        let doc = s
            .doc_upsert_with_opts(
                PROJECT,
                "a.md",
                "v1",
                DocUpsertOpts {
                    blob_hash: Some(old_blob.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        // Second upsert snapshots v1 (and old_blob) into store_doc_history,
        // which is what keeps old_blob referenced.
        s.doc_upsert_with_opts(
            PROJECT,
            "a.md",
            "v2",
            DocUpsertOpts {
                blob_hash: Some(new_blob.clone()),
                ..Default::default()
            },
        )
        .unwrap();
        s.doc_delete(&doc.id).unwrap();
        backdate_deletion(&s, &doc.id, DEFAULT_RETENTION_SECS + 60);

        s.gc(PROJECT, GcOpts::default()).unwrap();

        assert!(
            s.blob_get(&old_blob).unwrap().is_none(),
            "the history snapshot's blob outlived the row that referenced it"
        );
        assert_eq!(
            s.conn()
                .query_row(
                    "SELECT COUNT(*) FROM store_doc_history WHERE doc_id = ?1",
                    params![doc.id],
                    |row| row.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn eviction_drops_the_least_recently_updated_document_first() {
        let s = store();
        let old = s.doc_upsert(PROJECT, "old.md", &"x".repeat(400)).unwrap();
        let new = s.doc_upsert(PROJECT, "new.md", &"y".repeat(400)).unwrap();
        // doc_upsert stamps whole seconds, so two writes in one test land on
        // the same updated_at; make the intended victim explicitly older.
        s.conn()
            .execute(
                "UPDATE store_documents SET updated_at = updated_at - 3600 WHERE id = ?1",
                params![old.id],
            )
            .unwrap();

        let report = s
            .gc(
                PROJECT,
                GcOpts {
                    max_bytes: 500,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.evicted, 1);
        assert!(!row_exists(&s, &old.id));
        assert!(row_exists(&s, &new.id));
        assert!(report.bytes_after <= 500, "{report:?}");
    }

    #[test]
    fn eviction_releases_the_evicted_documents_blob() {
        let s = store();
        let hash = s.blob_store(&vec![7u8; 1024]).unwrap();
        s.doc_upsert_with_opts(
            PROJECT,
            "big.md",
            "",
            DocUpsertOpts {
                blob_hash: Some(hash.clone()),
                ..Default::default()
            },
        )
        .unwrap();

        let report = s
            .gc(
                PROJECT,
                GcOpts {
                    max_bytes: 64,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.evicted, 1);
        assert!(report.blobs_released >= 1);
        assert!(s.blob_get(&hash).unwrap().is_none());
    }

    #[test]
    fn under_budget_and_nothing_expired_is_a_no_op() {
        let s = store();
        let doc = s.doc_upsert(PROJECT, "keep.md", "small").unwrap();

        let report = s.gc(PROJECT, GcOpts::default()).unwrap();

        assert_eq!((report.purged, report.evicted), (0, 0));
        assert!(!report.vacuumed);
        assert!(row_exists(&s, &doc.id));
    }

    /// The rowid agreement is the real invariant: `doc_search` joins the
    /// index to the base table on rowid, so anything that renumbers one
    /// without the other silently empties search.
    ///
    /// Worth being straight about what this does and does not prove. The
    /// SQLite in use today leaves rowids alone across `VACUUM`, so this test
    /// still passes with the rebuild removed -- it pins the outcome, not the
    /// mechanism. The rebuild stays because SQLite documents renumbering as
    /// permitted for tables without an explicit INTEGER PRIMARY KEY (which
    /// `store_documents`, keyed on a TEXT id, is), and the failure mode if a
    /// future version starts exercising that permission is a search index
    /// that returns nothing with no error anywhere.
    #[test]
    fn documents_stay_searchable_after_a_vacuuming_gc() {
        let s = store();
        let victim = s
            .doc_upsert(PROJECT, "victim.md", "tombstoned text")
            .unwrap();
        s.doc_upsert(PROJECT, "keeper.md", "distinctive keeper text")
            .unwrap();
        s.doc_delete(&victim.id).unwrap();
        backdate_deletion(&s, &victim.id, DEFAULT_RETENTION_SECS + 60);

        let report = s.gc(PROJECT, GcOpts::default()).unwrap();
        assert!(report.vacuumed, "expected the one-time full VACUUM path");

        assert_eq!(rowids(&s, "store_documents"), rowids(&s, "store_docs_fts"));
        let hits = s.doc_search(PROJECT, "distinctive", 10).unwrap();
        assert_eq!(hits.len(), 1, "FTS index desynced by VACUUM's renumbering");
        assert!(hits[0].path == "keeper.md");
        assert!(s.doc_search(PROJECT, "tombstoned", 10).unwrap().is_empty());
    }
}
