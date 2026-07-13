use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};
use std::path::Path;

/// Schema history, oldest first. `0001_initial` is today's full schema —
/// every `CREATE TABLE`/`INDEX` uses `IF NOT EXISTS`, so replaying it against
/// a pre-migration db (one whose schema was applied via the old one-shot
/// `execute_batch`, and so has `user_version` still at 0) is a harmless
/// no-op that just brings it under migration tracking. Future schema changes
/// are new `000N_*.sql` files appended here, never edits to this one.
const MIGRATION_LIST: &[M<'static>] = &[M::up(include_str!("migrations/0001_initial.sql"))];
const MIGRATIONS: Migrations = Migrations::from_slice(MIGRATION_LIST);

pub fn open_db(path: &Path) -> Result<Connection, db_kit::open::Error> {
    let conn = db_kit::open_file(path, &MIGRATIONS)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

pub fn open_in_memory() -> Result<Connection, db_kit::open::Error> {
    let conn = db_kit::open_memory(&MIGRATIONS)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrate_is_safe_against_a_pre_migration_db() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("backend.db");

        // Simulate a db from before this crate adopted rusqlite_migration:
        // schema applied via one-shot execute_batch, user_version left at 0.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(include_str!("migrations/0001_initial.sql"))
                .unwrap();
            conn.execute(
                "INSERT INTO workspaces (id, name, slug, item_label, created_at, updated_at)
                 VALUES ('w1', 'Test', 'test', 'Item', 1, 1)",
                [],
            )
            .unwrap();
        }

        let conn = open_db(&path).unwrap();
        let name: String = conn
            .query_row("SELECT name FROM workspaces WHERE id = 'w1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "Test");
        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(user_version, 1);
    }

    #[test]
    fn open_db_sets_wal_journal_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let conn = open_db(&tmp.path().join("backend.db")).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }
}
