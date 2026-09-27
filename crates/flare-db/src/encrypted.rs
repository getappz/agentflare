use std::{fmt::Write as _, path::Path, sync::Arc, time::Duration};

use sqlx::{
    ConnectOptions, Connection, SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};

/// Bounded pool/lock waits. Contains no path or key and is safe to debug-print.
#[derive(Clone, Copy, Debug)]
pub struct EncryptedSqliteOptions {
    /// 1..=32 physical connections; default 5.
    pub max_connections: u32,
    /// Greater than zero and at most 60 seconds; default 5 seconds.
    pub acquire_timeout: Duration,
    /// Zero (fail immediately on contention) through 60 seconds; default 5 seconds.
    pub busy_timeout: Duration,
}

impl Default for EncryptedSqliteOptions {
    fn default() -> Self {
        Self {
            max_connections: 5,
            acquire_timeout: Duration::from_secs(5),
            busy_timeout: Duration::from_secs(5),
        }
    }
}

/// Opens a SQLCipher 4 database using a caller-owned, random 32-byte raw key.
///
/// Enable `sqlcipher` and link SQLCipher 4.19+ (major version 4). Without a supported
/// native library this returns an error before opening the target file.
/// `create_if_missing` must be explicitly enabled for first-time setup.
/// Existing plaintext databases are rejected, never converted in place.
///
/// Every connection is keyed before schema/page access and checked before
/// entering the pool (at most five connections). Migrations are a separate step
/// after this function succeeds. Statement logging is disabled to avoid logging
/// the key. The key is held in a private callback, outside debug-visible options.
/// Key generation, OS-keychain storage, permissions and backup policy belong to
/// the application. See the README for offline backup/restore requirements.
pub async fn connect_encrypted_sqlite(
    path: impl AsRef<Path>,
    key: &[u8],
    create_if_missing: bool,
) -> sqlx::Result<SqlitePool> {
    EncryptedSqliteOptions::default()
        .connect(path, key, create_if_missing)
        .await
}

impl EncryptedSqliteOptions {
    /// Opens the same validated pool as `connect_encrypted_sqlite` with bounded waits.
    pub async fn connect(
        self,
        path: impl AsRef<Path>,
        key: &[u8],
        create_if_missing: bool,
    ) -> sqlx::Result<SqlitePool> {
        if !(1..=32).contains(&self.max_connections)
            || self.acquire_timeout.is_zero()
            || self.acquire_timeout > Duration::from_secs(60)
            || self.busy_timeout > Duration::from_secs(60)
        {
            return Err(sqlx::Error::Configuration(
            "pool requires 1..=32 connections, acquisition timeout in (0, 60s], and busy timeout in [0, 60s]".into(),
        ));
        }
        if key.len() != 32 {
            return Err(sqlx::Error::Configuration(
                "SQLCipher requires a random 32-byte raw key".into(),
            ));
        }

        // SQLite silently ignores unknown PRAGMAs. Detect that before a target
        // file can be created by an ordinary, unencrypted SQLite build.
        let mut probe = SqliteConnection::connect("sqlite::memory:").await?;
        require_cipher(&mut probe).await?;
        probe.close().await?;

        // Only hex-encoded bytes enter SQL: never interpolate a passphrase or path.
        let mut key_sql = String::from("PRAGMA key = \"x'");
        for byte in key {
            write!(key_sql, "{byte:02x}").expect("writing to String cannot fail");
        }
        key_sql.push_str("'\"");
        let key_sql = Arc::new(key_sql);
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(create_if_missing)
            // SQLx 0.9's only initial SQL is foreign_keys=ON, a connection-local
            // flag with no schema/page access. Defer ALL page-dependent PRAGMAs
            // until after keying; never store the key in debug-visible options.
            .foreign_keys(true)
            .busy_timeout(self.busy_timeout)
            .disable_statement_logging();

        SqlitePoolOptions::new()
            .max_connections(self.max_connections)
            .acquire_timeout(self.acquire_timeout)
            .after_connect(move |connection, _| {
                let key_sql = Arc::clone(&key_sql);
                Box::pin(async move {
                    // Audited: only fixed SQL and 64 hex digits. Do not cache this
                    // secret-bearing statement or propagate its error text.
                    sqlx::query(sqlx::AssertSqlSafe(key_sql))
                        .persistent(false)
                        .execute(&mut *connection)
                        .await
                        .map_err(|_| {
                            sqlx::Error::Configuration(
                                "encrypted connection key initialization failed".into(),
                            )
                        })?;
                    sqlx::raw_sql(
                        "PRAGMA cipher_compatibility = 4;
                         PRAGMA cipher_plaintext_header_size = 0;",
                    )
                    .execute(&mut *connection)
                    .await?;
                    require_cipher(connection).await?;
                    // Setting a key alone does not authenticate an existing file.
                    sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sqlite_master")
                        .fetch_one(&mut *connection)
                        .await?;
                    sqlx::raw_sql(
                        "PRAGMA journal_mode = WAL;
                         PRAGMA synchronous = FULL;
                         PRAGMA temp_store = MEMORY;",
                    )
                    .execute(connection)
                    .await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await
    }
}

async fn require_cipher(connection: &mut SqliteConnection) -> sqlx::Result<()> {
    let version: Option<String> = sqlx::query_scalar("PRAGMA cipher_version")
        .fetch_optional(connection)
        .await?;
    if !version.is_some_and(|version| supported_cipher(&version)) {
        return Err(sqlx::Error::Configuration(
            "SQLCipher 4.19+ unavailable; enable sqlcipher and link a supported SQLCipher 4 library".into(),
        ));
    }
    Ok(())
}

fn supported_cipher(version: &str) -> bool {
    let mut numbers = version
        .split_whitespace()
        .next()
        .unwrap_or("")
        .split('.')
        .map(str::parse::<u32>);
    matches!((numbers.next(), numbers.next(), numbers.next(), numbers.next()),
        (Some(Ok(4)), Some(Ok(minor)), Some(Ok(_)), None) if minor >= 19)
}

#[test]
fn rejects_outdated_prerelease_and_unknown_ciphers() {
    for version in [
        "",
        "4.5.7 community",
        "4.18.0",
        "4.19",
        "4.19.0-beta",
        "5.0.0",
    ] {
        assert!(!supported_cipher(version), "{version}");
    }
    for version in ["4.19.0 community", "4.20.1"] {
        assert!(supported_cipher(version), "{version}");
    }
}
