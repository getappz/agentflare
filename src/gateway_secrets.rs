//! Encrypted storage for downstream gateway backend secrets (API tokens,
//! PATs). Reuses `auth_crypt`'s AES-256-GCM/PBKDF2 primitive directly — a
//! new table, NOT the `auth.rs`/`auth_db.rs` agent-CLI OAuth profile-
//! rotation vault, which is a different domain (per-agent profile
//! rotation/cooldowns/isolation vs. one secret per downstream MCP server).

use crate::auth_crypt;
use rusqlite::{Connection, OptionalExtension, params};

/// Creates the `gateway_secrets` table in an existing connection (called
/// from `db::open()` so the table lives in `agentflare.db`).
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS gateway_secrets (
            name TEXT PRIMARY KEY,
            ciphertext BLOB NOT NULL
        );",
    )
}

#[derive(Debug)]
pub enum SecretError {
    NoPassphrase,
    EncryptionFailed,
    WrongPassphrase,
    Sqlite(rusqlite::Error),
}

impl std::fmt::Display for SecretError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecretError::NoPassphrase => write!(
                f,
                "no vault passphrase available (set AGENTFLARE_VAULT_PASSPHRASE or run interactively)"
            ),
            SecretError::EncryptionFailed => write!(f, "encryption failed"),
            SecretError::WrongPassphrase => {
                write!(f, "wrong vault passphrase, or secret is corrupt")
            }
            SecretError::Sqlite(e) => write!(f, "gateway secrets database error: {e}"),
        }
    }
}

impl std::error::Error for SecretError {}

impl From<rusqlite::Error> for SecretError {
    fn from(e: rusqlite::Error) -> Self {
        SecretError::Sqlite(e)
    }
}

pub fn set_secret(conn: &Connection, name: &str, value: &str) -> Result<(), SecretError> {
    let passphrase = auth_crypt::get_passphrase().ok_or(SecretError::NoPassphrase)?;
    let ciphertext =
        auth_crypt::encrypt(value.as_bytes(), &passphrase).ok_or(SecretError::EncryptionFailed)?;
    conn.execute(
        "INSERT INTO gateway_secrets (name, ciphertext) VALUES (?1, ?2)
         ON CONFLICT(name) DO UPDATE SET ciphertext = excluded.ciphertext",
        params![name, ciphertext],
    )?;
    Ok(())
}

pub fn get_secret(conn: &Connection, name: &str) -> Result<Option<String>, SecretError> {
    let ciphertext: Option<Vec<u8>> = conn
        .query_row(
            "SELECT ciphertext FROM gateway_secrets WHERE name = ?1",
            params![name],
            |r| r.get(0),
        )
        .optional()?;
    let Some(ciphertext) = ciphertext else {
        return Ok(None);
    };
    let passphrase = auth_crypt::get_passphrase().ok_or(SecretError::NoPassphrase)?;
    let plaintext =
        auth_crypt::decrypt(&ciphertext, &passphrase).ok_or(SecretError::WrongPassphrase)?;
    String::from_utf8(plaintext)
        .map(Some)
        .map_err(|_| SecretError::WrongPassphrase)
}

pub fn list_secrets(conn: &Connection) -> Result<Vec<String>, SecretError> {
    let mut stmt = conn.prepare("SELECT name FROM gateway_secrets ORDER BY name")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn remove_secret(conn: &Connection, name: &str) -> Result<bool, SecretError> {
    let changed = conn.execute("DELETE FROM gateway_secrets WHERE name = ?1", params![name])?;
    Ok(changed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    // AGENTFLARE_VAULT_PASSPHRASE is process-global; cargo runs tests in this
    // binary in parallel by default, so the four tests below (each of which
    // sets/reads/removes it) must be serialized against each other and
    // against any other test in this binary that touches process env state.
    // Reuses the same lock src/paths.rs's test_support module already uses
    // for exactly this class of problem (see src/paths.rs:20-44).
    use agent_registry::detect::PATH_LOCK as GLOBAL_STATE_LOCK;

    fn set_passphrase(value: &str) {
        // SAFETY: caller holds GLOBAL_STATE_LOCK for the duration of the
        // test, so no other thread can read or write env vars concurrently.
        unsafe { std::env::set_var("AGENTFLARE_VAULT_PASSPHRASE", value) };
    }

    fn clear_passphrase() {
        // SAFETY: caller holds GLOBAL_STATE_LOCK for the duration of the test.
        unsafe { std::env::remove_var("AGENTFLARE_VAULT_PASSPHRASE") };
    }

    fn mem_migrated() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn roundtrip_set_get() {
        let _guard = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_passphrase("test-pass");
        let conn = mem_migrated();
        set_secret(&conn, "github_pat", "ghp_abc123").unwrap();
        assert_eq!(
            get_secret(&conn, "github_pat").unwrap(),
            Some("ghp_abc123".to_string())
        );
        clear_passphrase();
    }

    #[test]
    fn get_missing_secret_returns_none() {
        let _guard = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let conn = mem_migrated();
        assert_eq!(get_secret(&conn, "missing").unwrap(), None);
    }

    #[test]
    fn wrong_passphrase_is_a_clear_error_not_silent_empty_string() {
        let _guard = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_passphrase("right-pass");
        let conn = mem_migrated();
        set_secret(&conn, "s", "value").unwrap();
        set_passphrase("wrong-pass");
        let err = get_secret(&conn, "s").unwrap_err();
        assert!(matches!(err, SecretError::WrongPassphrase));
        clear_passphrase();
    }

    #[test]
    fn list_and_remove() {
        let _guard = GLOBAL_STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        set_passphrase("test-pass");
        let conn = mem_migrated();
        set_secret(&conn, "a", "1").unwrap();
        set_secret(&conn, "b", "2").unwrap();
        assert_eq!(
            list_secrets(&conn).unwrap(),
            vec!["a".to_string(), "b".to_string()]
        );
        assert!(remove_secret(&conn, "a").unwrap());
        assert!(!remove_secret(&conn, "a").unwrap());
        assert_eq!(list_secrets(&conn).unwrap(), vec!["b".to_string()]);
        clear_passphrase();
    }
}
