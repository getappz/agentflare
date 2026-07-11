//! The single source-of-truth SQLite database, `~/.agentflare/agentflare.db`.
//! New relational state adds a table + migration here rather than a new file
//! (see #138 — gateway secrets fold in later). The rebuildable caches under
//! `~/.local/share/agentflare/` (skills index, gateway tool-index) stay
//! separate: they belong in the data dir, not next to source-of-truth state.
use rusqlite::Connection;
use std::path::PathBuf;
use std::time::Duration;

pub fn agentflare_db_path() -> PathBuf {
    crate::paths::home().join(".agentflare").join("agentflare.db")
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
    Ok(conn)
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
