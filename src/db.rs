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
    crate::gateway_secrets::migrate(&conn)?;
    // One-time migration: copy secrets from old gateway.db
    // (pre-#138 separate file) into agentflare.db.
    migrate_old_gateway_db(&conn)?;
    Ok(conn)
}

/// If `~/.agentflare/gateway.db` exists with a `gateway_secrets` table,
/// copy rows into `agentflare.db` that don't already exist (so re-running
/// the migration is idempotent). Leaves the old file in place.
fn migrate_old_gateway_db(conn: &Connection) -> rusqlite::Result<()> {
    let old_path = old_gateway_db_path();
    if !old_path.exists() {
        return Ok(());
    }
    // Probe: does the old file have a gateway_secrets table with rows?
    let old = match Connection::open(&old_path) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };
    let count: i64 = match old.query_row(
        "SELECT COUNT(*) FROM gateway_secrets",
        [],
        |r| r.get(0),
    ) {
        Ok(n) => n,
        Err(_) => return Ok(()),
    };
    if count == 0 {
        return Ok(());
    }
    // Copy each row that doesn't already exist in the new db.
    let mut stmt = old.prepare("SELECT name, ciphertext FROM gateway_secrets")?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
    })?;
    for row in rows {
        let (name, ciphertext) = row?;
        conn.execute(
            "INSERT OR IGNORE INTO gateway_secrets (name, ciphertext) VALUES (?1, ?2)",
            params![name, ciphertext],
        )?;
    }
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
/// write is in flight.
fn tune(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    // journal_mode returns a row; query_row consumes it. WAL is a no-op on
    // in-memory dbs (tests), which is fine.
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    Ok(())
}
