use crate::Store;
use std::path::Path;

const MIGRATION_MARKER: &str = "_migration_state_json_v1";

#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Store error: {0}")]
    Store(#[from] rusqlite::Error),
    #[error("migration already ran at {0}")]
    AlreadyRan(i64),
}

pub fn migrate_state_json(store: &Store, path: &Path) -> Result<usize, MigrateError> {
    if let Some(existing) = store.kv_get(MIGRATION_MARKER)? {
        let ts: i64 = serde_json::from_slice(&existing.value)?;
        return Err(MigrateError::AlreadyRan(ts));
    }

    let content = std::fs::read_to_string(path)?;
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&content)?;

    // One transaction for every key plus the completion marker: a partial
    // failure rolls back entirely instead of leaving the marker set (or
    // absent) out of sync with which keys actually landed.
    let conn = store.conn();
    let tx = rusqlite::Transaction::new_unchecked(&conn, rusqlite::TransactionBehavior::Immediate)?;
    let now = db_kit::ids::now();
    let upsert = |key: &str, value: &[u8]| -> Result<(), MigrateError> {
        tx.execute(
            "INSERT INTO store_kv (key, value, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = ?3",
            rusqlite::params![key, value, now],
        )?;
        Ok(())
    };

    for (key, value) in &map {
        upsert(key, &serde_json::to_vec(value)?)?;
    }
    upsert(MIGRATION_MARKER, &serde_json::to_vec(&now)?)?;

    tx.commit()?;
    Ok(map.len())
}

pub fn validate_roundtrip(store: &Store, path: &Path) -> Result<bool, MigrateError> {
    let content = std::fs::read_to_string(path)?;
    let original: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&content)?;

    for (key, expected) in &original {
        let entry = store.kv_get(key)?;
        match entry {
            None => return Ok(false),
            Some(entry) => {
                let stored: serde_json::Value = serde_json::from_slice(&entry.value)?;
                if &stored != expected {
                    return Ok(false);
                }
            }
        }
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    #[test]
    fn migrate_and_validate() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        std::fs::write(&state_path, r#"{"hello":"world","count":42,"flag":true}"#).unwrap();

        let db_path = dir.path().join("store.db");
        let store = Store::open_file(&db_path).unwrap();

        let count = migrate_state_json(&store, &state_path).unwrap();
        assert_eq!(count, 3);

        let hello = store.kv_get("hello").unwrap().unwrap();
        let val: serde_json::Value = serde_json::from_slice(&hello.value).unwrap();
        assert_eq!(val, serde_json::json!("world"));

        assert!(validate_roundtrip(&store, &state_path).unwrap());
    }

    #[test]
    fn rejects_repeat_migration() {
        let dir = tempfile::tempdir().unwrap();
        let state_path = dir.path().join("state.json");
        std::fs::write(&state_path, r#"{"k":"v"}"#).unwrap();

        let store = Store::open_memory().unwrap();
        migrate_state_json(&store, &state_path).unwrap();

        let err = migrate_state_json(&store, &state_path).unwrap_err();
        assert!(matches!(err, MigrateError::AlreadyRan(_)));
    }
}
