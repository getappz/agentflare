use rusqlite_migration::{M, Migrations};

/// Uniqueness on (project_id, path) was previously enforced only by the
/// single-writer application code path, so an existing database could
/// already hold duplicate rows -- `CREATE UNIQUE INDEX` fails outright
/// against those, breaking migration (and thus startup) for anyone who
/// happens to have one. Keep the most-recently-written row per
/// (project_id, path) and drop the rest first. Exposed as a constant (not
/// inlined into the migration list below) so the dedup behavior itself can
/// be tested directly against a minimal schema, without needing to
/// replay/version-track the full production migration sequence.
///
/// Discarding a document row is not a single DELETE, because two things
/// outlive it:
///
/// * `store_doc_history.doc_id` references `store_documents(id)`. This
///   connection never enables `PRAGMA foreign_keys` (see
///   `db_kit::open_file`), so the delete does not fail -- it silently
///   strands history rows that no longer name a live document, and nothing
///   in the codebase ever deletes them.
/// * `store_blobs.ref_count` is a stored counter, not a live scan over
///   referencing rows, so a row dropped without decrementing pins its blob
///   at a positive count forever.
///
/// Hence: release the references first, then the history, then the rows.
/// The decrement is per reference removed rather than per distinct hash --
/// several discarded rows can point at one blob. Blobs whose count reaches
/// zero keep their file on disk; reclaiming those is disk-level GC, which
/// this migration deliberately leaves to the cache-eviction work rather
/// than doing file I/O from a SQL migration.
pub(crate) const DEDUP_AND_UNIQUE_INDEX_MIGRATION: &str = "
    UPDATE store_blobs
       SET ref_count = MAX(0, ref_count
           - (SELECT COUNT(*) FROM store_documents d
               WHERE d.blob_hash = store_blobs.hash
                 AND d.rowid NOT IN (
                     SELECT MAX(rowid) FROM store_documents GROUP BY project_id, path))
           - (SELECT COUNT(*) FROM store_doc_history h
                JOIN store_documents d ON d.id = h.doc_id
               WHERE h.blob_hash = store_blobs.hash
                 AND d.rowid NOT IN (
                     SELECT MAX(rowid) FROM store_documents GROUP BY project_id, path)));

    DELETE FROM store_doc_history
    WHERE doc_id IN (
        SELECT id FROM store_documents
        WHERE rowid NOT IN (
            SELECT MAX(rowid) FROM store_documents GROUP BY project_id, path
        )
    );

    DELETE FROM store_documents
    WHERE rowid NOT IN (
        SELECT MAX(rowid) FROM store_documents GROUP BY project_id, path
    );

    CREATE UNIQUE INDEX IF NOT EXISTS idx_docs_project_path ON store_documents(project_id, path);
";

/// Converts `store_docs_fts` from a standalone fts5 table kept in sync by
/// hand into an external-content one driven by triggers.
///
/// The manual scheme put an FTS write next to every `store_documents` write,
/// across two crates -- and a delete path that forgot one is exactly the bug
/// #334/#337 fixed (soft-deleted rows stayed matchable). Triggers make the
/// FTS write a property of the base table instead of something each new call
/// site has to remember, and external content drops the index's duplicate
/// copy of every document's text.
///
/// Soft-deleted rows stay out of the index, preserving what the manual code
/// did. That is why the triggers are guarded rather than the straight
/// mirrors in `src/memory/schema.rs`: for an external-content table the
/// `'delete'` command must only ever name a row that is actually indexed, so
/// resurrecting a soft-deleted document (an UPDATE whose `old.deleted_at` is
/// set) must not try to delete an entry that was already removed.
///
/// `UPDATE OF content, deleted_at` and not a bare `UPDATE`: those are the
/// only two columns the index depends on, and `doc_upsert_with_opts` follows
/// its content write with up to nine single-column UPDATEs for the optional
/// fields, each of which would otherwise re-tokenize the whole document.
///
/// Note for the cache-eviction work (#339): `store_documents` has a TEXT
/// primary key, so its rowids are implicit and `VACUUM` may renumber them --
/// which would desync this index (and `doc_search`'s rowid JOIN, which had
/// the same coupling before this migration). Anything that VACUUMs must call
/// [`crate::Store::doc_fts_rebuild`] afterwards.
pub(crate) const EXTERNAL_CONTENT_FTS_MIGRATION: &str = "
    DROP TABLE IF EXISTS store_docs_fts;

    CREATE VIRTUAL TABLE store_docs_fts USING fts5(
        content,
        content='store_documents'
    );

    CREATE TRIGGER store_docs_fts_ai AFTER INSERT ON store_documents BEGIN
        INSERT INTO store_docs_fts(rowid, content)
            SELECT new.rowid, new.content WHERE new.deleted_at IS NULL;
    END;

    CREATE TRIGGER store_docs_fts_ad AFTER DELETE ON store_documents BEGIN
        INSERT INTO store_docs_fts(store_docs_fts, rowid, content)
            SELECT 'delete', old.rowid, old.content WHERE old.deleted_at IS NULL;
    END;

    CREATE TRIGGER store_docs_fts_au AFTER UPDATE OF content, deleted_at ON store_documents BEGIN
        INSERT INTO store_docs_fts(store_docs_fts, rowid, content)
            SELECT 'delete', old.rowid, old.content WHERE old.deleted_at IS NULL;
        INSERT INTO store_docs_fts(rowid, content)
            SELECT new.rowid, new.content WHERE new.deleted_at IS NULL;
    END;

    INSERT INTO store_docs_fts(rowid, content)
        SELECT rowid, content FROM store_documents WHERE deleted_at IS NULL;
";

/// Repopulates `store_docs_fts` from `store_documents`. Not `'rebuild'`,
/// which would index the soft-deleted rows the triggers deliberately keep
/// out. See [`EXTERNAL_CONTENT_FTS_MIGRATION`].
pub(crate) const FTS_REBUILD_SQL: &str = "
    INSERT INTO store_docs_fts(store_docs_fts) VALUES('delete-all');
    INSERT INTO store_docs_fts(rowid, content)
        SELECT rowid, content FROM store_documents WHERE deleted_at IS NULL;
";

pub fn migrations() -> Migrations<'static> {
    Migrations::new(vec![
        M::up(
            "CREATE TABLE IF NOT EXISTS store_kv (
            key   TEXT PRIMARY KEY NOT NULL,
            value BLOB NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS store_documents (
            id         TEXT PRIMARY KEY NOT NULL,
            project_id TEXT NOT NULL DEFAULT '',
            path       TEXT NOT NULL,
            content    TEXT NOT NULL DEFAULT '',
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            deleted_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_docs_project ON store_documents(project_id);",
        ),
        M::up(
            "CREATE VIRTUAL TABLE IF NOT EXISTS store_docs_fts USING fts5(
            content
        );",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS store_docs_vec (
            doc_id TEXT PRIMARY KEY NOT NULL,
            embedding BLOB NOT NULL,
            updated_at INTEGER NOT NULL
        );",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS store_blobs (
            hash    TEXT PRIMARY KEY NOT NULL,
            size    INTEGER NOT NULL,
            ref_count INTEGER NOT NULL DEFAULT 1,
            created_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS store_blob_chunks (
            hash        TEXT NOT NULL,
            chunk_index INTEGER NOT NULL,
            data        BLOB NOT NULL,
            PRIMARY KEY (hash, chunk_index)
        );",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS store_leases (
            key          TEXT PRIMARY KEY NOT NULL,
            owner        TEXT NOT NULL,
            status       TEXT NOT NULL DEFAULT 'claimed',
            created_at   INTEGER NOT NULL,
            heartbeat_at INTEGER NOT NULL
        );",
        ),
        M::up(
            "ALTER TABLE store_documents ADD COLUMN title TEXT NOT NULL DEFAULT '';
             ALTER TABLE store_documents ADD COLUMN doc_type TEXT NOT NULL DEFAULT 'file';
             ALTER TABLE store_documents ADD COLUMN blob_hash TEXT;
             ALTER TABLE store_documents ADD COLUMN mime TEXT NOT NULL DEFAULT '';
             ALTER TABLE store_documents ADD COLUMN tags TEXT NOT NULL DEFAULT '[]';
             ALTER TABLE store_documents ADD COLUMN session_id TEXT;
             ALTER TABLE store_documents ADD COLUMN source TEXT NOT NULL DEFAULT '';
             ALTER TABLE store_documents ADD COLUMN version INTEGER NOT NULL DEFAULT 1;",
        ),
        M::up(
            "CREATE TABLE IF NOT EXISTS store_doc_history (
            id         TEXT PRIMARY KEY NOT NULL,
            doc_id     TEXT NOT NULL REFERENCES store_documents(id),
            version    INTEGER NOT NULL,
            content    TEXT NOT NULL DEFAULT '',
            blob_hash  TEXT,
            mime       TEXT NOT NULL DEFAULT '',
            title      TEXT NOT NULL DEFAULT '',
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_doc_history_doc ON store_doc_history(doc_id);",
        ),
        M::up(
            "ALTER TABLE store_documents ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';
             ALTER TABLE store_documents ADD COLUMN size INTEGER NOT NULL DEFAULT 0;
             ALTER TABLE store_doc_history ADD COLUMN metadata TEXT NOT NULL DEFAULT '{}';
             ALTER TABLE store_doc_history ADD COLUMN size INTEGER NOT NULL DEFAULT 0;",
        ),
        M::up(DEDUP_AND_UNIQUE_INDEX_MIGRATION),
        M::up(EXTERNAL_CONTENT_FTS_MIGRATION),
    ])
}

#[cfg(test)]
mod fts_migration_tests {
    use super::EXTERNAL_CONTENT_FTS_MIGRATION;

    /// The pre-migration shape: a standalone fts5 table with its own copy of
    /// the text, and `store_documents` alongside it.
    fn legacy(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "CREATE TABLE store_documents (
                id         TEXT PRIMARY KEY NOT NULL,
                project_id TEXT NOT NULL DEFAULT '',
                path       TEXT NOT NULL,
                content    TEXT NOT NULL DEFAULT '',
                deleted_at INTEGER
            );
            CREATE VIRTUAL TABLE store_docs_fts USING fts5(content);",
        )
        .unwrap();
    }

    fn hits(conn: &rusqlite::Connection, term: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM store_docs_fts WHERE store_docs_fts MATCH ?1",
            [term],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[test]
    fn migration_reindexes_live_rows_and_leaves_soft_deleted_ones_out() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        legacy(&conn);
        conn.execute_batch(
            "INSERT INTO store_documents (id, path, content) VALUES ('a', '/live', 'kept');
             INSERT INTO store_documents (id, path, content, deleted_at)
                VALUES ('b', '/dead', 'dropped', 1);
             INSERT INTO store_docs_fts(rowid, content) VALUES (1, 'kept');",
        )
        .unwrap();

        conn.execute_batch(EXTERNAL_CONTENT_FTS_MIGRATION).unwrap();

        assert_eq!(hits(&conn, "kept"), 1, "live rows must survive the rebuild");
        assert_eq!(
            hits(&conn, "dropped"),
            0,
            "the migration must not index rows the triggers would keep out"
        );
    }

    #[test]
    fn migration_leaves_an_index_that_is_external_content_and_trigger_driven() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        legacy(&conn);
        conn.execute_batch(EXTERNAL_CONTENT_FTS_MIGRATION).unwrap();

        let sql: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name = 'store_docs_fts'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(
            sql.contains("content='store_documents'"),
            "index must read its text back from the base table, not duplicate it: {sql}"
        );

        let triggers: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                  WHERE type = 'trigger' AND tbl_name = 'store_documents'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(triggers, 3, "insert, delete and update must all be covered");
    }

    #[test]
    fn a_row_written_after_the_migration_needs_no_explicit_fts_write() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        legacy(&conn);
        conn.execute_batch(EXTERNAL_CONTENT_FTS_MIGRATION).unwrap();

        // Deliberately only the base-table write -- the whole point is that a
        // call site can no longer forget the other half.
        conn.execute_batch(
            "INSERT INTO store_documents (id, path, content) VALUES ('a', '/new', 'freshly');",
        )
        .unwrap();
        assert_eq!(hits(&conn, "freshly"), 1);

        conn.execute_batch("UPDATE store_documents SET deleted_at = 1 WHERE id = 'a';")
            .unwrap();
        assert_eq!(hits(&conn, "freshly"), 0);
    }
}

#[cfg(test)]
mod tests {
    use super::DEDUP_AND_UNIQUE_INDEX_MIGRATION;

    /// The dependent schema the migration actually has to reckon with:
    /// `store_doc_history` carries the real foreign key onto
    /// `store_documents(id)`, and `store_blobs` carries the stored
    /// `ref_count` that a dropped row would otherwise pin. Foreign keys are
    /// left at SQLite's default (off) on purpose -- that is how
    /// `db_kit::open_file` opens the production database, so a test that
    /// turned them on would be testing a configuration that never runs.
    fn seed(conn: &rusqlite::Connection) {
        conn.execute_batch(
            "CREATE TABLE store_documents (
                id         TEXT PRIMARY KEY NOT NULL,
                project_id TEXT NOT NULL DEFAULT '',
                path       TEXT NOT NULL,
                content    TEXT NOT NULL DEFAULT '',
                blob_hash  TEXT
            );
            CREATE TABLE store_doc_history (
                id         TEXT PRIMARY KEY NOT NULL,
                doc_id     TEXT NOT NULL REFERENCES store_documents(id),
                version    INTEGER NOT NULL DEFAULT 1,
                blob_hash  TEXT
            );
            CREATE TABLE store_blobs (
                hash       TEXT PRIMARY KEY NOT NULL,
                ref_count  INTEGER NOT NULL DEFAULT 0
            );",
        )
        .unwrap();
    }

    #[test]
    fn dedup_migration_keeps_newest_row_and_enables_the_unique_index() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        seed(&conn);

        // Simulate two legacy duplicate rows for the same (project_id, path)
        // -- exactly what the old, app-only-enforced uniqueness could let
        // through before this migration existed.
        conn.execute(
            "INSERT INTO store_documents (id, project_id, path, content) VALUES ('a', 'p', '/dup', 'old')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO store_documents (id, project_id, path, content) VALUES ('b', 'p', '/dup', 'new')",
            [],
        )
        .unwrap();

        // Applying the real dedup+unique-index SQL must not fail against
        // pre-existing duplicates, and must keep the most-recently-inserted
        // row (highest rowid).
        conn.execute_batch(DEDUP_AND_UNIQUE_INDEX_MIGRATION)
            .unwrap();

        let remaining: Vec<String> = conn
            .prepare("SELECT id FROM store_documents WHERE project_id = 'p' AND path = '/dup'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            remaining,
            vec!["b".to_string()],
            "must keep the most-recently-inserted duplicate"
        );

        // The unique index must now actually be enforced.
        let dup_insert = conn.execute(
            "INSERT INTO store_documents (id, project_id, path, content) VALUES ('c', 'p', '/dup', 'x')",
            [],
        );
        assert!(
            dup_insert.is_err(),
            "unique index must reject a new duplicate after the migration"
        );
    }

    #[test]
    fn dedup_migration_drops_history_of_discarded_rows_and_keeps_the_survivor_s() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        seed(&conn);
        conn.execute_batch(
            "INSERT INTO store_documents (id, project_id, path) VALUES ('a', 'p', '/dup');
             INSERT INTO store_documents (id, project_id, path) VALUES ('b', 'p', '/dup');
             INSERT INTO store_doc_history (id, doc_id, version) VALUES ('h1', 'a', 1);
             INSERT INTO store_doc_history (id, doc_id, version) VALUES ('h2', 'a', 2);
             INSERT INTO store_doc_history (id, doc_id, version) VALUES ('h3', 'b', 1);",
        )
        .unwrap();

        conn.execute_batch(DEDUP_AND_UNIQUE_INDEX_MIGRATION)
            .unwrap();

        // Without the history delete these rows would survive pointing at a
        // document id that no longer exists, unreachable and never cleaned
        // up by anything.
        let orphans: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM store_doc_history h
                  WHERE NOT EXISTS (SELECT 1 FROM store_documents d WHERE d.id = h.doc_id)",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0, "no history may outlive the document it names");

        let kept: Vec<String> = conn
            .prepare("SELECT id FROM store_doc_history ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            kept,
            vec!["h3".to_string()],
            "the surviving document must keep its own history"
        );
    }

    #[test]
    fn dedup_migration_releases_blob_refs_held_by_discarded_rows() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        seed(&conn);
        // One blob referenced four times: by both duplicates and by a
        // history row of each. Only the two references belonging to the
        // discarded row ('a', the lower rowid) may be released -- a naive
        // per-distinct-hash decrement would subtract 1 instead of 2.
        conn.execute_batch(
            "INSERT INTO store_blobs (hash, ref_count) VALUES ('deadbeef', 4);
             INSERT INTO store_documents (id, project_id, path, blob_hash) VALUES ('a', 'p', '/dup', 'deadbeef');
             INSERT INTO store_documents (id, project_id, path, blob_hash) VALUES ('b', 'p', '/dup', 'deadbeef');
             INSERT INTO store_doc_history (id, doc_id, blob_hash) VALUES ('h1', 'a', 'deadbeef');
             INSERT INTO store_doc_history (id, doc_id, blob_hash) VALUES ('h2', 'b', 'deadbeef');",
        )
        .unwrap();

        conn.execute_batch(DEDUP_AND_UNIQUE_INDEX_MIGRATION)
            .unwrap();

        let refs: i64 = conn
            .query_row(
                "SELECT ref_count FROM store_blobs WHERE hash = 'deadbeef'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            refs, 2,
            "exactly the references the discarded row and its history held \
             must be released, leaving the survivor's two intact"
        );
    }

    #[test]
    fn dedup_migration_is_a_no_op_on_a_store_without_duplicates() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        seed(&conn);
        conn.execute_batch(
            "INSERT INTO store_blobs (hash, ref_count) VALUES ('deadbeef', 1);
             INSERT INTO store_documents (id, project_id, path, blob_hash) VALUES ('a', 'p', '/one', 'deadbeef');
             INSERT INTO store_documents (id, project_id, path) VALUES ('b', 'p', '/two');
             INSERT INTO store_doc_history (id, doc_id, version) VALUES ('h1', 'a', 1);",
        )
        .unwrap();

        conn.execute_batch(DEDUP_AND_UNIQUE_INDEX_MIGRATION)
            .unwrap();

        // The overwhelmingly common case: a database that was always clean
        // must come through untouched, blob counts included.
        let (docs, history, refs): (i64, i64, i64) = conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM store_documents),
                        (SELECT COUNT(*) FROM store_doc_history),
                        (SELECT ref_count FROM store_blobs WHERE hash = 'deadbeef')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            (docs, history, refs),
            (2, 1, 1),
            "a store with no duplicates must be left exactly as it was"
        );
    }
}
