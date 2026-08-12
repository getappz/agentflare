//! Connection-open boilerplate: create parent dir, open, busy timeout, WAL,
//! run the caller's migrations. Callers own their own `Migrations` — this
//! module doesn't know about anyone's schema, it just runs whichever
//! migration list is handed to it.

use rusqlite::Connection;
use rusqlite_migration::{MigrationDefinitionError, Migrations};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Migration(#[from] rusqlite_migration::Error),
    /// The database's recorded schema version is higher than the newest
    /// migration this binary knows about. Always means a *different* build
    /// of agentflare (usually installed over the shared binary via
    /// `dev-install` from a work-in-progress branch) already applied a
    /// migration that hasn't reached the branch this binary was built from
    /// -- rusqlite_migration has no downgrade path, so it refuses to touch
    /// the database rather than guess.
    #[error(
        "{path} is at schema version {applied}, newer than any migration this \
         agentflare build knows about. This means a different (often WIP/unmerged) \
         build applied a migration -- usually via `dev-install` from a branch that \
         adds a `migrations/NNNN_*.sql` file not yet on this binary's branch. Fix: \
         find and merge/cherry-pick that migration file (and its registration in \
         this crate's `db.rs`), then rebuild. Do not edit `PRAGMA user_version` by \
         hand -- the schema on disk already reflects the missing migration."
    )]
    SchemaAhead { path: PathBuf, applied: i64 },
}

/// Runs `migrations.to_latest`, upgrading the generic "too high" error into
/// [`Error::SchemaAhead`] with the actual recorded version so the message is
/// actionable instead of a bare rusqlite_migration string with no next step.
fn migrate(path: &Path, conn: &mut Connection, migrations: &Migrations) -> Result<(), Error> {
    match migrations.to_latest(conn) {
        Ok(()) => Ok(()),
        Err(rusqlite_migration::Error::MigrationDefinition(
            MigrationDefinitionError::DatabaseTooFarAhead,
        )) => {
            let applied: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            Err(Error::SchemaAhead {
                path: path.to_path_buf(),
                applied,
            })
        }
        Err(e) => Err(e.into()),
    }
}

pub fn open_file(path: &Path, migrations: &Migrations) -> Result<Connection, Error> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    // Ahead of journal_mode, and that ordering is load-bearing: auto_vacuum
    // can only be chosen while the database file has no pages yet, and
    // switching to WAL writes the header. Set it after, and the pragma is
    // silently ignored -- the read-back still says INCREMENTAL until the
    // first table lands, then reverts to NONE.
    //
    // A database created before this line existed stays in NONE mode
    // regardless; one full `VACUUM` converts it, which is what
    // `agentflare_store::Store::gc` does the first time it has pages to
    // reclaim.
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // The durability level WAL is designed around: a crash can lose the last
    // few transactions, a power loss still cannot corrupt the file. FULL
    // fsyncs every commit, which for local agent stores buys nothing over
    // WAL's own guarantees.
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    enforce_foreign_keys(&conn)?;
    migrate(path, &mut conn, migrations)?;
    Ok(conn)
}

pub fn open_memory(migrations: &Migrations) -> Result<Connection, Error> {
    let mut conn = Connection::open_in_memory()?;
    enforce_foreign_keys(&conn)?;
    migrations.to_latest(&mut conn)?;
    Ok(conn)
}

/// Makes every `REFERENCES` in the caller's schema actually enforced.
///
/// The bundled SQLite this crate links already defaults foreign keys ON, so
/// today this changes nothing — which is the point of stating it: the
/// guarantee should come from the code rather than from a build flag of the
/// `libsqlite3-sys` build that happens to be linked, and every schema here
/// was written against enforcement being live (`store_doc_history` ->
/// `store_documents`, `memory_relations` -> `observations`).
fn enforce_foreign_keys(conn: &Connection) -> Result<(), Error> {
    conn.pragma_update(None, "foreign_keys", true)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite_migration::M;

    const PARENT_CHILD: &str = "CREATE TABLE parent (id INTEGER PRIMARY KEY);
         CREATE TABLE child (parent_id INTEGER NOT NULL REFERENCES parent(id));";

    fn migrations() -> Migrations<'static> {
        Migrations::new(vec![M::up(PARENT_CHILD)])
    }

    /// Read back through a connection that has already created tables, not
    /// straight after the pragma: an ignored `auto_vacuum` still reads as
    /// INCREMENTAL on an empty file and only reverts once a page exists, so
    /// checking it too early passes no matter what order the pragmas run in.
    #[test]
    fn a_new_file_database_reclaims_pages_and_relaxes_sync() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        drop(open_file(&path, &migrations()).unwrap());

        let conn = open_file(&path, &migrations()).unwrap();
        let auto_vacuum: i64 = conn
            .query_row("PRAGMA auto_vacuum", [], |row| row.get(0))
            .unwrap();
        let synchronous: i64 = conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(auto_vacuum, 2, "expected INCREMENTAL");
        assert_eq!(synchronous, 1, "expected NORMAL");
    }

    #[test]
    fn references_are_enforced_on_runtime_writes() {
        let conn = open_memory(&migrations()).unwrap();
        let err = conn
            .execute("INSERT INTO child (parent_id) VALUES (1)", [])
            .unwrap_err();
        assert_eq!(
            err.sqlite_error_code(),
            Some(rusqlite::ErrorCode::ConstraintViolation),
            "{err}"
        );
    }

    /// Enforcement is live during the migrations too, which is why a
    /// migration that removes parent rows has to clear their children first
    /// -- the store's dedup migration already does. Pinned here because the
    /// obvious "turn it on afterwards" alternative would quietly relax what
    /// migrations are allowed to do.
    #[test]
    fn references_are_enforced_during_the_migrations() {
        let migrations = Migrations::new(vec![
            M::up(PARENT_CHILD),
            M::up(
                "INSERT INTO parent (id) VALUES (1);
                   INSERT INTO child (parent_id) VALUES (1);",
            ),
            M::up("DELETE FROM parent WHERE id = 1;"),
        ]);
        assert!(open_memory(&migrations).is_err());
    }

    /// Reproduces the real-world failure this crate exists to make
    /// diagnosable: a DB previously opened by a build with more migrations
    /// (e.g. a WIP branch dev-installed over the shared binary) than the
    /// build now opening it knows about. Must surface as `SchemaAhead` with
    /// the actual recorded version, not the bare upstream "too high" string.
    #[test]
    fn a_database_migrated_by_a_newer_build_reports_schema_ahead() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");

        let ahead = Migrations::new(vec![M::up(PARENT_CHILD), M::up("-- pretend v2")]);
        drop(open_file(&path, &ahead).unwrap());

        let err = open_file(&path, &migrations()).unwrap_err();
        match err {
            Error::SchemaAhead { applied, .. } => assert_eq!(applied, 2),
            other => panic!("expected SchemaAhead, got {other:?}"),
        }
    }
}
