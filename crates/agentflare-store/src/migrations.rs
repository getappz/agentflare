use rusqlite_migration::{M, Migrations};

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
    ])
}
