use crate::Store;
use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct KvEntry {
    pub key: String,
    pub value: Vec<u8>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl Store {
    pub fn kv_set(&self, key: &str, value: &[u8]) -> rusqlite::Result<()> {
        let now = db_kit::ids::now();
        self.conn.execute(
            "INSERT INTO store_kv (key, value, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
            params![key, value, now],
        )?;
        Ok(())
    }

    pub fn kv_get(&self, key: &str) -> rusqlite::Result<Option<KvEntry>> {
        self.conn
            .query_row(
                "SELECT key, value, created_at, updated_at FROM store_kv WHERE key = ?1",
                params![key],
                |row| {
                    Ok(KvEntry {
                        key: row.get(0)?,
                        value: row.get(1)?,
                        created_at: row.get(2)?,
                        updated_at: row.get(3)?,
                    })
                },
            )
            .optional()
    }

    pub fn kv_exists(&self, key: &str) -> rusqlite::Result<bool> {
        self.conn
            .query_row(
                "SELECT 1 FROM store_kv WHERE key = ?1",
                params![key],
                |_| Ok(()),
            )
            .optional()
            .map(|o| o.is_some())
    }

    pub fn kv_delete(&self, key: &str) -> rusqlite::Result<bool> {
        let n = self
            .conn
            .execute("DELETE FROM store_kv WHERE key = ?1", params![key])?;
        Ok(n > 0)
    }

    pub fn kv_scan(&self, prefix: &str) -> rusqlite::Result<Vec<KvEntry>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value, created_at, updated_at FROM store_kv WHERE key >= ?1 AND key < ?2 ORDER BY key")?;
        let end = {
            let mut s = prefix.to_string();
            s.push('\u{10FFFF}');
            s
        };
        let rows = stmt.query_map(params![prefix, end], |row| {
            Ok(KvEntry {
                key: row.get(0)?,
                value: row.get(1)?,
                created_at: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> Store {
        Store::open_memory().unwrap()
    }

    #[test]
    fn set_and_get() {
        let s = store();
        s.kv_set("hello", b"world").unwrap();
        let entry = s.kv_get("hello").unwrap().unwrap();
        assert_eq!(entry.key, "hello");
        assert_eq!(entry.value, b"world");
    }

    #[test]
    fn get_missing() {
        let s = store();
        assert!(s.kv_get("nope").unwrap().is_none());
    }

    #[test]
    fn exists() {
        let s = store();
        assert!(!s.kv_exists("x").unwrap());
        s.kv_set("x", b"1").unwrap();
        assert!(s.kv_exists("x").unwrap());
    }

    #[test]
    fn delete() {
        let s = store();
        s.kv_set("x", b"1").unwrap();
        assert!(s.kv_delete("x").unwrap());
        assert!(!s.kv_exists("x").unwrap());
    }

    #[test]
    fn scan_prefix() {
        let s = store();
        s.kv_set("a:1", b"").unwrap();
        s.kv_set("a:2", b"").unwrap();
        s.kv_set("b:1", b"").unwrap();
        let entries = s.kv_scan("a:").unwrap();
        assert_eq!(entries.len(), 2);
    }
}
