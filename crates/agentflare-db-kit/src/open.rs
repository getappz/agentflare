//! Connection-open boilerplate: create parent dir, open, busy timeout, WAL,
//! run the caller's migrations. Callers own their own `Migrations` — this
//! module doesn't know about anyone's schema, it just runs whichever
//! migration list is handed to it.

use rusqlite::Connection;
use rusqlite_migration::Migrations;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Migration(#[from] rusqlite_migration::Error),
}

pub fn open_file(path: &Path, migrations: &Migrations) -> Result<Connection, Error> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_millis(5000))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    migrations.to_latest(&mut conn)?;
    Ok(conn)
}

pub fn open_memory(migrations: &Migrations) -> Result<Connection, Error> {
    let mut conn = Connection::open_in_memory()?;
    migrations.to_latest(&mut conn)?;
    Ok(conn)
}
