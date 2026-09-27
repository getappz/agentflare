#![cfg(feature = "sqlite")]

use flare_db::connect_encrypted_sqlite;

#[tokio::test]
async fn missing_or_malformed_keys_never_create_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing.db");
    for key in [vec![], vec![1; 31], vec![1; 33]] {
        assert!(connect_encrypted_sqlite(&path, &key, true).await.is_err());
        assert!(!path.exists());
    }
    for options in [
        flare_db::EncryptedSqliteOptions {
            max_connections: 0,
            ..Default::default()
        },
        flare_db::EncryptedSqliteOptions {
            max_connections: 33,
            ..Default::default()
        },
        flare_db::EncryptedSqliteOptions {
            acquire_timeout: std::time::Duration::ZERO,
            ..Default::default()
        },
        flare_db::EncryptedSqliteOptions {
            busy_timeout: std::time::Duration::from_secs(61),
            ..Default::default()
        },
    ] {
        assert!(options.connect(&path, &[42; 32], true).await.is_err());
        assert!(!path.exists());
    }
}

#[cfg(not(feature = "sqlcipher"))]
#[tokio::test]
async fn missing_cipher_fails_before_opening_target() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("must-not-be-plaintext.db");
    let error = connect_encrypted_sqlite(&path, &[42; 32], true)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("SQLCipher 4.19+ unavailable"));
    assert!(!path.exists());
}

#[cfg(feature = "sqlcipher")]
#[tokio::test]
async fn encrypted_pool_reopen_wrong_key_plaintext_rejection_and_offline_backup() {
    use sqlx::{Connection, sqlite::SqliteConnectOptions};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("encrypted.db");
    let key = [42; 32]; // Test fixture only; applications supply CSPRNG keys.
    assert!(connect_encrypted_sqlite(&path, &key, false).await.is_err());
    assert!(!path.exists());
    let pool = connect_encrypted_sqlite(&path, &key, true).await.unwrap();
    let cipher_version: String = sqlx::query_scalar("PRAGMA cipher_version")
        .fetch_one(&pool)
        .await
        .unwrap();
    eprintln!("SQLCipher version: {cipher_version}");
    assert!(!format!("{pool:?}").contains(&"2a".repeat(32)));
    static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
    flare_db::run_migrations(&pool, &MIGRATOR).await.unwrap();
    let marker = "CONFIDENTIAL-FLARE-DB-ENCRYPTION-TEST";
    sqlx::query("INSERT INTO posts(title, body) VALUES (?, ?)")
        .bind(marker)
        .bind(marker)
        .execute(&pool)
        .await
        .unwrap();

    // Hold all five handles concurrently: this forces independent connections,
    // not repeated checkout of the same already-keyed connection.
    let mut connections = Vec::new();
    for _ in 0..5 {
        let mut connection = pool.acquire().await.unwrap();
        let value: String = sqlx::query_scalar("SELECT title FROM posts")
            .fetch_one(&mut *connection)
            .await
            .unwrap();
        assert_eq!(value, marker);
        connections.push(connection);
    }
    let wal = std::fs::read(dir.path().join("encrypted.db-wal")).unwrap();
    assert!(
        !wal.windows(marker.len())
            .any(|bytes| bytes == marker.as_bytes())
    );
    drop(connections);
    let checkpoint: (i64, i64, i64) = sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(checkpoint.0, 0);
    pool.close().await;
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.starts_with(b"SQLite format 3\0"));
    assert!(
        !bytes
            .windows(marker.len())
            .any(|bytes| bytes == marker.as_bytes())
    );

    let error = connect_encrypted_sqlite(&path, &[99; 32], false)
        .await
        .unwrap_err();
    assert!(!format!("{error:?} {error}").contains(&"63".repeat(32)));
    assert!(
        std::fs::read(&path).unwrap() == bytes,
        "wrong-key open changed checkpointed database bytes"
    );
    let plain_options = SqliteConnectOptions::new().filename(&path);
    let mut unkeyed = sqlx::SqliteConnection::connect_with(&plain_options)
        .await
        .unwrap();
    assert!(
        sqlx::query("SELECT * FROM posts")
            .fetch_all(&mut unkeyed)
            .await
            .is_err()
    );
    unkeyed.close().await.unwrap();
    assert!(
        std::fs::read(&path).unwrap() == bytes,
        "unkeyed open changed checkpointed database bytes"
    );

    // The supported backup is a copy made only after all connections close.
    let backup = dir.path().join("backup.db");
    std::fs::copy(&path, &backup).unwrap();
    let restored = connect_encrypted_sqlite(&backup, &key, false)
        .await
        .unwrap();
    flare_db::run_migrations(&restored, &MIGRATOR)
        .await
        .unwrap();
    let value: String = sqlx::query_scalar("SELECT title FROM posts")
        .fetch_one(&restored)
        .await
        .unwrap();
    assert_eq!(value, marker);
    restored.close().await;
    assert!(
        connect_encrypted_sqlite(&backup, &[99; 32], false)
            .await
            .is_err()
    );
    let reopened = connect_encrypted_sqlite(&path, &key, false).await.unwrap();
    reopened.close().await;

    let plaintext = dir.path().join("plaintext.db");
    let options = SqliteConnectOptions::new()
        .filename(&plaintext)
        .create_if_missing(true);
    let mut connection = sqlx::SqliteConnection::connect_with(&options)
        .await
        .unwrap();
    sqlx::query("CREATE TABLE keep_me(id INTEGER)")
        .execute(&mut connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let before = std::fs::read(&plaintext).unwrap();
    assert!(
        connect_encrypted_sqlite(&plaintext, &key, true)
            .await
            .is_err()
    );
    assert!(
        std::fs::read(&plaintext).unwrap() == before,
        "plaintext rejection modified the source"
    );
}
