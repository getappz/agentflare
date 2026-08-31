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
use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
    /// Only blobs this project's live documents actually reference count.
    /// `store_blobs` is shared across projects in one database, and summing
    /// it whole would let another project's bytes push this one over its
    /// budget — eviction would then run against a project that is not the
    /// one consuming the space, and could empty it entirely without ever
    /// getting under a target it never controlled.
    pub fn cache_bytes(&self, project_id: &str) -> rusqlite::Result<u64> {
        let conn = self.conn();
        let content: i64 = conn.query_row(
            "SELECT COALESCE(SUM(length(CAST(content AS BLOB))), 0) FROM store_documents
             WHERE project_id = ?1 AND deleted_at IS NULL",
            params![project_id],
            |row| row.get(0),
        )?;
        let blobs: i64 = conn.query_row(
            "SELECT COALESCE(SUM(b.size), 0) FROM store_blobs b
             WHERE EXISTS (SELECT 1 FROM store_documents d
                            WHERE d.blob_hash = b.hash
                              AND d.project_id = ?1 AND d.deleted_at IS NULL)",
            params![project_id],
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

        // Re-measured after every eviction instead of estimated once for the
        // whole batch. Two documents can share a blob, which is only
        // reclaimed when the second of them goes, so any per-document
        // estimate taken before the deletions start under-counts what the
        // batch will free — and an estimate that can never reach its target
        // walks the candidate list to the end and empties the project. The
        // extra queries only run while over budget.
        while self.cache_bytes(project_id)? > opts.max_bytes {
            let Some(id) = self.next_eviction_candidate(project_id)? else {
                break;
            };
            report.blobs_released += self.hard_delete_docs(std::slice::from_ref(&id), true)?;
            report.evicted += 1;
        }

        if report.purged + report.evicted > 0 {
            report.vacuumed = self.reclaim()?;
        }
        report.bytes_after = self.cache_bytes(project_id)?;
        Ok(report)
    }

    /// The next live document to drop, or `None` once the project holds
    /// none — which is what bounds the eviction loop.
    ///
    /// `updated_at` order, not true LRU: a real last-accessed column costs a
    /// write on every read, and for a cache that is refreshed rather than
    /// mutated the two orders differ very little.
    fn next_eviction_candidate(&self, project_id: &str) -> rusqlite::Result<Option<String>> {
        let conn = self.conn();
        conn.query_row(
            "SELECT id FROM store_documents
             WHERE project_id = ?1 AND deleted_at IS NULL
             ORDER BY updated_at ASC, rowid ASC LIMIT 1",
            params![project_id],
            |row| row.get(0),
        )
        .optional()
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

                // store_doc_history and store_doc_chunks both have
                // REFERENCES on store_documents, so with foreign keys
                // enforced this ordering is required. store_chunk_vec
                // references store_doc_chunks, so it must go first.
                tx.execute(
                    "DELETE FROM store_doc_history WHERE doc_id = ?1",
                    params![id],
                )?;
                tx.execute("DELETE FROM store_docs_vec WHERE doc_id = ?1", params![id])?;
                tx.execute(
                    "DELETE FROM store_chunk_vec WHERE chunk_id IN (SELECT id FROM store_doc_chunks WHERE doc_id = ?1)",
                    params![id],
                )?;
                tx.execute("DELETE FROM store_doc_chunks WHERE doc_id = ?1", params![id])?;
                // Dropping the row out of store_docs_fts is the AFTER DELETE
                // trigger's job — see migrations::EXTERNAL_CONTENT_FTS_MIGRATION.
                // store_chunks_fts is similarly handled by its own triggers on store_doc_chunks.
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

/// What one [`sweep_legacy_blobs`] run reclaimed.
#[derive(Debug, Default, Clone, Serialize)]
pub struct LegacySweepReport {
    /// Files found in the legacy directory.
    pub scanned: usize,
    pub reclaimed: usize,
    pub bytes_reclaimed: u64,
    /// File names of the databases whose references were counted.
    pub databases: Vec<String>,
    /// True when nothing was actually deleted.
    pub dry_run: bool,
}

/// Removes files from the pre-namespacing shared `<root>/blobs` that no
/// database in `root` still references.
///
/// Blob directories are per-database now (`Store::blob_dir`): each `.db` keeps
/// its own `store_blobs` ref counts, so a shared directory let one store's
/// reclaim strand another store's content. Writes and deletes moved to the
/// namespaced directory and reads fall back to this one, which is why nothing
/// cleans it up in the normal course of things — only a sweep that consults
/// *every* database in the directory can prove a file here is dead.
///
/// Aborts without deleting anything if a database cannot be read: an
/// incomplete reference set would make live content look like garbage.
pub fn sweep_legacy_blobs(root: &Path, dry_run: bool) -> Result<LegacySweepReport, crate::Error> {
    let legacy = root.join("blobs");
    let mut report = LegacySweepReport {
        dry_run,
        ..Default::default()
    };
    if !legacy.is_dir() {
        return Ok(report);
    }

    // An entry that fails to read is propagated rather than skipped: it could
    // be a store database, and silently dropping one from the reference set is
    // what turns another store's live content into apparent garbage.
    let mut dbs: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.extension().is_some_and(|ext| ext == "db") {
            dbs.push(path);
        }
    }
    dbs.sort();

    let referenced = collect_referenced_hashes(&dbs, &mut report.databases)?;

    // With no *store* database consulted, every file would look unreferenced.
    // `.db` files that turned out not to be stores prove nothing, so this is
    // checked after the collect rather than on the raw `.db` count.
    if report.databases.is_empty() {
        return Err(crate::Error::NotFound(format!(
            "no store databases in {} — refusing to sweep",
            root.display()
        )));
    }

    for shard in std::fs::read_dir(&legacy)? {
        let shard = shard?.path();
        if !shard.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&shard)? {
            let entry = entry?;
            let path = entry.path();
            let Some(hash) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            report.scanned += 1;
            if referenced.contains(hash) {
                continue;
            }
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            if !dry_run {
                std::fs::remove_file(&path)?;
            }
            report.reclaimed += 1;
            report.bytes_reclaimed += size;
        }
        // An emptied shard is clutter; a non-empty one just stays.
        if !dry_run {
            let _ = std::fs::remove_dir(&shard);
        }
    }

    Ok(report)
}

/// Every blob hash referenced by any store database in `dbs`, appending the
/// consulted file names to `consulted`.
fn collect_referenced_hashes(
    dbs: &[PathBuf],
    consulted: &mut Vec<String>,
) -> Result<HashSet<String>, crate::Error> {
    let mut referenced = HashSet::new();
    for db in dbs {
        // Read-only on purpose, and no migrations: the sweep must not alter a
        // database it is only consulting.
        let conn =
            rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        // A `.db` without the table isn't a store database and holds no
        // references — checked explicitly rather than by swallowing a failed
        // query, so a database that is broken rather than unrelated still
        // propagates instead of silently contributing nothing.
        let is_store = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'store_blobs'",
                [],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !is_store {
            continue;
        }
        let mut stmt = conn.prepare("SELECT hash FROM store_blobs")?;
        for hash in stmt.query_map([], |row| row.get::<_, String>(0))? {
            referenced.insert(hash?);
        }
        consulted.push(db.file_name().unwrap_or_default().to_string_lossy().into());
    }
    Ok(referenced)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::documents::DocUpsertOpts;

    const PROJECT: &str = "p";

    fn store() -> Store {
        Store::open_memory().unwrap()
    }

    /// Writes a file into the legacy shared `<root>/blobs` the way a build
    /// predating per-database blob directories would have.
    fn legacy_file(root: &Path, hash: &str, data: &[u8]) -> PathBuf {
        let path = root.join("blobs").join(&hash[..2]).join(hash);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, data).unwrap();
        path
    }

    /// Registers `hash` in `s`'s ref-count table without writing a file --
    /// the row is what the sweep consults.
    fn reference(s: &Store, hash: &str) {
        s.conn()
            .execute(
                "INSERT INTO store_blobs (hash, size, ref_count, created_at) VALUES (?1, 1, 1, ?2)",
                params![hash, db_kit::ids::now()],
            )
            .unwrap();
    }

    const KEPT: &str = "aa00000000000000000000000000000000000000000000000000000000000001";
    const DEAD: &str = "bb00000000000000000000000000000000000000000000000000000000000002";

    #[test]
    fn the_sweep_reclaims_only_files_no_database_in_the_directory_references() {
        let dir = tempfile::tempdir().unwrap();
        let docs = Store::open_file(&dir.path().join("flare-docs.db")).unwrap();
        let _assets = Store::open_file(&dir.path().join("store.db")).unwrap();
        // Referenced by one of the two databases; the sweep must consult both.
        reference(&docs, KEPT);

        let kept = legacy_file(dir.path(), KEPT, b"still referenced");
        let dead = legacy_file(dir.path(), DEAD, b"orphaned by the split");

        let report = sweep_legacy_blobs(dir.path(), false).unwrap();

        assert!(kept.exists(), "a referenced legacy file must survive");
        assert!(
            !dead.exists(),
            "an unreferenced legacy file must be removed"
        );
        assert_eq!((report.scanned, report.reclaimed), (2, 1));
        assert_eq!(
            report.bytes_reclaimed,
            b"orphaned by the split".len() as u64
        );
        assert_eq!(report.databases.len(), 2, "both databases consulted");
    }

    #[test]
    fn a_dry_run_reports_what_it_would_reclaim_without_deleting() {
        let dir = tempfile::tempdir().unwrap();
        let _s = Store::open_file(&dir.path().join("store.db")).unwrap();
        let dead = legacy_file(dir.path(), DEAD, b"orphan");

        let report = sweep_legacy_blobs(dir.path(), true).unwrap();

        assert!(dead.exists(), "a dry run must not delete");
        assert_eq!(report.reclaimed, 1);
        assert!(report.dry_run);
    }

    // Without a database there is no reference set, so every file would look
    // dead -- exactly the situation in which deleting is unrecoverable.
    #[test]
    fn the_sweep_refuses_to_run_with_no_database_to_consult() {
        let dir = tempfile::tempdir().unwrap();
        let orphan = legacy_file(dir.path(), DEAD, b"unprovable");

        assert!(sweep_legacy_blobs(dir.path(), false).is_err());
        assert!(orphan.exists(), "a refused sweep must delete nothing");
    }

    // A directory full of `.db` files none of which is a store is the same
    // evidential hole as an empty one: the reference set is empty either way.
    #[test]
    fn the_sweep_refuses_when_no_database_present_is_a_store() {
        let dir = tempfile::tempdir().unwrap();
        let other = rusqlite::Connection::open(dir.path().join("unrelated.db")).unwrap();
        other
            .execute_batch("CREATE TABLE notes (id INTEGER)")
            .unwrap();
        drop(other);
        let orphan = legacy_file(dir.path(), DEAD, b"unprovable");

        assert!(sweep_legacy_blobs(dir.path(), false).is_err());
        assert!(orphan.exists(), "a refused sweep must delete nothing");
    }

    #[test]
    fn an_unrelated_database_in_the_directory_is_skipped_rather_than_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let s = Store::open_file(&dir.path().join("store.db")).unwrap();
        reference(&s, KEPT);
        let kept = legacy_file(dir.path(), KEPT, b"still referenced");

        // A sqlite file that isn't a store: no store_blobs table.
        let other = rusqlite::Connection::open(dir.path().join("unrelated.db")).unwrap();
        other
            .execute_batch("CREATE TABLE notes (id INTEGER)")
            .unwrap();
        drop(other);

        let report = sweep_legacy_blobs(dir.path(), false).unwrap();

        assert!(kept.exists());
        assert_eq!(
            report.databases,
            vec!["store.db".to_string()],
            "only the store database contributes references"
        );
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

    /// `store_blobs` is shared across projects in one database. Summing it
    /// whole made another project's bytes the trigger for this one's
    /// eviction — and since evicting this project's documents never brought
    /// that total down, it emptied the project and still finished over
    /// budget.
    #[test]
    fn another_projects_blobs_do_not_evict_this_projects_documents() {
        let s = store();
        let hash = s.blob_store(&vec![9u8; 4096]).unwrap();
        s.doc_upsert_with_opts(
            "other",
            "big.md",
            "",
            DocUpsertOpts {
                blob_hash: Some(hash),
                ..Default::default()
            },
        )
        .unwrap();
        let mine = s.doc_upsert(PROJECT, "small.md", "tiny").unwrap();

        let report = s
            .gc(
                PROJECT,
                GcOpts {
                    max_bytes: 1024,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.evicted, 0);
        assert!(row_exists(&s, &mine.id));
    }

    /// A blob two documents share is only reclaimed when the second one
    /// goes, so an up-front per-document estimate under-counts what the
    /// batch frees. The loop re-measures instead: one eviction is not
    /// enough here, two are, and the third document must survive.
    #[test]
    fn eviction_re_measures_rather_than_estimating_a_shared_blob() {
        let s = store();
        let payload = vec![3u8; 2048];
        let mut shared = String::new();
        for path in ["a.md", "b.md"] {
            // One `blob_store` per document, the way every real caller
            // writes one: identical bytes dedupe to a single row whose
            // ref_count counts the referrers.
            shared = s.blob_store(&payload).unwrap();
            s.doc_upsert_with_opts(
                PROJECT,
                path,
                "",
                DocUpsertOpts {
                    blob_hash: Some(shared.clone()),
                    ..Default::default()
                },
            )
            .unwrap();
        }
        let keeper = s.doc_upsert(PROJECT, "c.md", "tiny").unwrap();
        s.conn()
            .execute(
                "UPDATE store_documents SET updated_at = updated_at + 3600 WHERE id = ?1",
                params![keeper.id],
            )
            .unwrap();

        let report = s
            .gc(
                PROJECT,
                GcOpts {
                    max_bytes: 1024,
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(report.evicted, 2);
        assert!(row_exists(&s, &keeper.id));
        assert!(s.blob_get(&shared).unwrap().is_none());
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
