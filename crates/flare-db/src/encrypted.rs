use std::{fmt::Write as _, path::Path, time::Duration};

use sqlx::{
    ConnectOptions, Connection, SqliteConnection, SqlitePool,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions},
};

/// Opens a SQLCipher 4 database using a caller-owned, random 32-byte raw key.
///
/// Enable `sqlcipher` and link SQLCipher 4.19+ (major version 4). Without a supported
/// native library this returns an error before opening the target file.
/// `create_if_missing` must be explicitly enabled for first-time setup.
/// Existing plaintext databases are rejected, never converted in place.
///
/// Every connection is keyed before SQLx's other PRAGMAs and checked before
/// entering the pool (at most five connections). Migrations are a separate step
/// after this function succeeds. Statement logging is disabled to avoid logging
/// the key. Do not log `pool.connect_options()`: SQLx retains the key there.
/// Key generation, OS-keychain storage, permissions and backup policy belong to
/// the application. See the README for offline backup/restore requirements.
pub async fn connect_encrypted_sqlite(
    path: impl AsRef<Path>,
    key: &[u8],
    create_if_missing: bool,
) -> sqlx::Result<SqlitePool> {
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
    let mut literal = String::with_capacity(69);
    literal.push_str("\"x'");
    for byte in key {
        write!(literal, "{byte:02x}").expect("writing to String cannot fail");
    }
    literal.push_str("'\"");
    let options = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create_if_missing)
        // SQLx reserves the key and cipher PRAGMAs ahead of all page access.
        .pragma("key", literal)
        .pragma("cipher_compatibility", "4")
        .pragma("cipher_plaintext_header_size", "0")
        .pragma("temp_store", "MEMORY")
        .journal_mode(SqliteJournalMode::Wal)
        .disable_statement_logging();

    SqlitePoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .after_connect(|connection, _| {
            Box::pin(async move {
                require_cipher(connection).await?;
                // Setting a key alone does not authenticate an existing file.
                sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sqlite_master")
                    .fetch_one(connection)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options)
        .await
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
