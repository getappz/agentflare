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
pub(crate) const DEDUP_AND_UNIQUE_INDEX_MIGRATION: &str = "
    DELETE FROM store_documents
    WHERE rowid NOT IN (
        SELECT MAX(rowid) FROM store_documents GROUP BY project_id, path
    );
    CREATE UNIQUE INDEX IF NOT EXISTS idx_docs_project_path ON store_documents(project_id, path);
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
    ])
}

#[cfg(test)]
mod tests {
    use super::DEDUP_AND_UNIQUE_INDEX_MIGRATION;

    #[test]
    fn dedup_migration_keeps_newest_row_and_enables_the_unique_index() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE store_documents (
                id         TEXT PRIMARY KEY NOT NULL,
                project_id TEXT NOT NULL DEFAULT '',
                path       TEXT NOT NULL,
                content    TEXT NOT NULL DEFAULT ''
            );",
        )
        .unwrap();

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
}
