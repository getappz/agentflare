//! The single source-of-truth SQLite database, `~/.agentflare/agentflare.db`.
//! New relational state adds a table + migration here rather than a new file
//! (see #138 — gateway secrets fold in later). The rebuildable caches under
//! `~/.local/share/agentflare/` (skills index, gateway tool-index) stay
//! separate: they belong in the data dir, not next to source-of-truth state.
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::time::Duration;

pub fn agentflare_db_path() -> PathBuf {
    crate::paths::home().join(".agentflare").join("agentflare.db")
}

fn old_gateway_db_path() -> PathBuf {
    crate::paths::home().join(".agentflare").join("gateway.db")
}

/// Opens (creating if absent) `agentflare.db` and applies every table's
/// migration. Each subsystem owns its own `CREATE TABLE IF NOT EXISTS` so
/// this stays a thin dispatcher.
pub fn open() -> rusqlite::Result<Connection> {
    let path = agentflare_db_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
        restrict(parent, 0o700);
    }
    let conn = Connection::open(&path)?;
    // The db holds coordination state now and gateway secrets after #138 —
    // keep it owner-only rather than SQLite's umask-masked 0644 default.
    restrict(&path, 0o600);
    tune(&conn)?;
    crate::claims::migrate(&conn)?;
    crate::review::migrate(&conn)?;
    crate::gateway_secrets::migrate(&conn)?;
    // One-time migration: copy secrets from old gateway.db
    // (pre-#138 separate file) into agentflare.db.
    migrate_old_gateway_db(&conn)?;
    Ok(conn)
}

fn migrate_old_gateway_db(conn: &Connection) -> rusqlite::Result<()> {
    import_legacy_secrets(conn, &old_gateway_db_path())
}

/// Copy secrets from a legacy `gateway.db` (pre-#138 separate file) into the
/// shared db, then rename the legacy file so it's imported exactly once.
///
/// Reads from the legacy db are best-effort: a missing file, an unopenable
/// db, or an incompatible/malformed `gateway_secrets` schema all skip the
/// import rather than failing `open()` — otherwise one bad legacy file would
/// brick unrelated claims access too. Only writes into our own (healthy)
/// shared db are fatal.
///
/// Renaming to `gateway.db.migrated` on success is the migration-complete
/// marker: without it every `open()` re-imports via `INSERT OR IGNORE`, so a
/// secret the user deleted would resurrect on the next run.
fn import_legacy_secrets(conn: &Connection, old_path: &std::path::Path) -> rusqlite::Result<()> {
    if !old_path.exists() {
        return Ok(());
    }
    let Ok(old) = Connection::open(old_path) else {
        return Ok(());
    };
    let Ok(mut stmt) = old.prepare("SELECT name, ciphertext FROM gateway_secrets") else {
        return Ok(()); // incompatible legacy schema — skip, don't brick open()
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
    }) else {
        return Ok(());
    };
    // `flatten` drops rows that fail to decode (e.g. malformed ciphertext)
    // rather than propagating — one bad row must not abort the migration.
    for (name, ciphertext) in rows.flatten() {
        conn.execute(
            "INSERT OR IGNORE INTO gateway_secrets (name, ciphertext) VALUES (?1, ?2)",
            params![name, ciphertext],
        )?;
    }
    // Best-effort: if the rename fails we just re-import idempotently next time.
    drop(stmt);
    drop(old);
    let _ = std::fs::rename(old_path, old_path.with_extension("db.migrated"));
    Ok(())
}

/// Best-effort owner-only permissions (no-op off Unix).
#[cfg(unix)]
fn restrict(path: &std::path::Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
}

#[cfg(not(unix))]
fn restrict(_path: &std::path::Path, _mode: u32) {}

/// Concurrency settings for a multi-writer ledger: many agent processes hit
/// this db at once. Without a busy timeout, a contended write returns
/// SQLITE_BUSY immediately and an acquire surfaces as "database is locked"
/// instead of serializing behind the current writer — so a 5s timeout lets
/// writers wait their turn. WAL lets readers (`claim_list`) proceed while a
/// write is in flight. synchronous=NORMAL is safe under WAL and avoids the
/// fsync-on-every-commit cost of FULL.
fn tune(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    // journal_mode returns a row; query_row consumes it. WAL is a no-op on
    // in-memory dbs (tests), which is fine.
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::gateway_secrets::migrate(&conn).unwrap();
        conn
    }

    fn write_legacy(path: &std::path::Path, ddl: &str, rows: &[(&str, &[u8])]) {
        let old = Connection::open(path).unwrap();
        old.execute_batch(ddl).unwrap();
        for (name, ct) in rows {
            old.execute(
                "INSERT INTO gateway_secrets (name, ciphertext) VALUES (?1, ?2)",
                params![name, ct],
            )
            .unwrap();
        }
    }

    const GOOD_DDL: &str =
        "CREATE TABLE gateway_secrets (name TEXT PRIMARY KEY, ciphertext BLOB NOT NULL);";

    #[test]
    fn deleted_secret_stays_deleted_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let old_path = dir.path().join("gateway.db");
        write_legacy(&old_path, GOOD_DDL, &[("github_pat", b"cipher")]);

        let conn = new_db();
        import_legacy_secrets(&conn, &old_path).unwrap();
        let present: i64 = conn
            .query_row("SELECT COUNT(*) FROM gateway_secrets WHERE name='github_pat'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(present, 1, "secret should import on first open");

        // Legacy file is renamed once migrated, so a re-import can't resurrect.
        assert!(!old_path.exists(), "legacy file should be renamed after migration");
        conn.execute("DELETE FROM gateway_secrets WHERE name='github_pat'", []).unwrap();
        import_legacy_secrets(&conn, &old_path).unwrap();
        let after: i64 = conn
            .query_row("SELECT COUNT(*) FROM gateway_secrets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after, 0, "deleted secret must not resurrect on reopen");
    }

    #[test]
    fn incompatible_legacy_schema_does_not_brick_open() {
        let dir = tempfile::tempdir().unwrap();
        let old_path = dir.path().join("gateway.db");
        // gateway_secrets exists but lacks the `ciphertext` column.
        let old = Connection::open(&old_path).unwrap();
        old.execute_batch("CREATE TABLE gateway_secrets (name TEXT, blob BLOB);").unwrap();
        old.execute("INSERT INTO gateway_secrets (name, blob) VALUES ('x', X'00')", []).unwrap();
        drop(old);

        let conn = new_db();
        // Must be Ok — a malformed legacy schema is skipped, not propagated.
        import_legacy_secrets(&conn, &old_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM gateway_secrets", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn missing_legacy_file_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let conn = new_db();
        import_legacy_secrets(&conn, &dir.path().join("nope.db")).unwrap();
    }
}
