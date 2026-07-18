use rusqlite::Connection;
use std::path::PathBuf;

pub fn brain_db_path() -> PathBuf {
    crate::paths::home()
        .join(".agentflare")
        .join("memory")
        .join("brain.db")
}

/// Wraps a non-rusqlite error (e.g. a filesystem error) so it can propagate
/// through `rusqlite::Result` without silently discarding it.
fn io_err(e: std::io::Error) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(e))
}

pub fn open() -> rusqlite::Result<Connection> {
    let path = brain_db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io_err)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(io_err)?;
        }
    }
    let conn = db_kit::open_file(&path, &super::schema::migrations()).map_err(|e| match e {
        db_kit::open::Error::Sqlite(s) => s,
        other => rusqlite::Error::ToSqlConversionFailure(Box::new(other)),
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(io_err)?;
    }
    // db-kit sets busy_timeout + WAL; the brain additionally needs these —
    // foreign_keys especially, since memory_relations relies on REFERENCES
    // enforcement and SQLite defaults it OFF.
    conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::test_support::with_temp_home;

    // A brain.db created by the pre-migration engine (raw execute_batch DDL,
    // user_version=0) must open cleanly through the db-kit path, keep its
    // data, and advance user_version. Same precedent as agentflare-backend's
    // migrate_is_safe_against_a_pre_migration_db.
    #[test]
    fn legacy_brain_db_upgrades_cleanly() {
        with_temp_home(|| {
            let path = brain_db_path();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let legacy = rusqlite::Connection::open(&path).unwrap();
            legacy.execute_batch(super::super::schema::V1_DDL).unwrap();
            legacy
                .execute(
                    "INSERT INTO observations (type, title, content, created_at, updated_at)
                     VALUES ('note', 'legacy', 'row', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
                    [],
                )
                .unwrap();
            drop(legacy);

            let conn = open().unwrap();
            let n: i64 = conn
                .query_row("SELECT COUNT(*) FROM observations", [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 1);
            let v: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert!(v >= 1, "user_version must advance, got {v}");
            let mode: String = conn
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .unwrap();
            assert_eq!(mode.to_lowercase(), "wal");
            let fk: i64 = conn
                .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                .unwrap();
            assert_eq!(fk, 1, "foreign key enforcement must stay on");
        });
    }
}
