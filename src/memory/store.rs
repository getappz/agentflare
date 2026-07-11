use rusqlite::Connection;
use std::path::PathBuf;
use std::time::Duration;

pub fn brain_db_path() -> PathBuf {
    crate::paths::home().join(".agentflare").join("memory").join("brain.db")
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
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(io_err)?;
        }
    }
    let conn = Connection::open(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(io_err)?;
    }
    tune(&conn)?;
    super::schema::migrate(&conn)?;
    Ok(conn)
}

fn tune(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    let _: String = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))?;
    conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
    Ok(())
}
