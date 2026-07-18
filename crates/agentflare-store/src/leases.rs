use db_kit::claim::ClaimLedger;
use rusqlite::Connection;

pub static LEDGER: ClaimLedger = ClaimLedger::new("store_leases", &["key"]);

pub fn acquire(
    conn: &Connection,
    key: &str,
    owner: &str,
    ttl_secs: i64,
) -> rusqlite::Result<Acquire> {
    let now = db_kit::ids::now();
    LEDGER.acquire(conn, &[key], owner, now, ttl_secs)
}

pub use db_kit::claim::Acquire;

pub fn heartbeat(conn: &Connection, key: &str, owner: &str) -> rusqlite::Result<bool> {
    let now = db_kit::ids::now();
    LEDGER.heartbeat(conn, &[key], owner, now)
}

pub fn release(conn: &Connection, key: &str, owner: &str) -> rusqlite::Result<bool> {
    LEDGER.release(conn, &[key], owner)
}

pub fn is_owner(conn: &Connection, key: &str, owner: &str) -> rusqlite::Result<bool> {
    LEDGER.is_owner(conn, &[key], owner)
}

pub fn done(conn: &Connection, key: &str, owner: &str) -> rusqlite::Result<bool> {
    let now = db_kit::ids::now();
    LEDGER.done(conn, &[key], owner, now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use db_kit::open_file;

    #[test]
    fn acquire_and_release_cycle() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let conn = open_file(&path, &super::super::migrations::migrations()).unwrap();

        let key = "test-key";
        let owner = "test-owner";

        assert_eq!(acquire(&conn, key, owner, 60).unwrap(), Acquire::Acquired);
        assert!(heartbeat(&conn, key, owner).unwrap());
        assert!(release(&conn, key, owner).unwrap());
        assert!(!is_owner(&conn, key, owner).unwrap());
    }
}
