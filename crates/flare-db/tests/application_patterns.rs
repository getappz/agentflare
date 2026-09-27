//! Reuse one encrypted pool; keep application rules in the caller.
#![cfg(feature = "sqlcipher")]

use flare_db::{Crud, EncryptedSqliteOptions, Pool, sqlx};
use sqlx::{
    Acquire, SqlSafeStr,
    migrate::{Migration, MigrationType, Migrator},
};
use std::time::Duration;

#[derive(sqlx::FromRow, Crud)]
#[crud(table = "records", pk = "id")]
struct Record {
    id: i64,
    tenant_id: i64,
    value: i64,
    row_version: i64,
}

// An application operation shared by desktop services and a future server.
async fn apply(pool: &Pool, request: &str, fail_late: bool) -> sqlx::Result<i64> {
    let mut tx = pool.begin().await?;
    let row = Record::create_one(
        &mut *tx,
        RecordNew {
            tenant_id: 1,
            value: 10,
            row_version: 0,
        },
    )
    .await?;
    sqlx::query("INSERT INTO audit(record_id) VALUES (?)")
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO outbox(tenant_id, request_key, record_id) VALUES (?, ?, ?)")
        .bind(row.tenant_id)
        .bind(request)
        .bind(row.id)
        .execute(&mut *tx)
        .await?;
    if fail_late {
        // A constraint failure after all writes rolls back all three tables.
        sqlx::query("INSERT INTO audit(record_id) VALUES (-1)")
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(row.id)
}

#[tokio::test]
async fn shared_pool_atomic_outbox_optimistic_updates_and_idempotency() {
    let dir = tempfile::tempdir().unwrap();
    let options = EncryptedSqliteOptions {
        max_connections: 2,
        busy_timeout: Duration::from_millis(200),
        // Initial native crypto startup also counts toward pool acquisition.
        acquire_timeout: Duration::from_secs(5),
    };
    let pool = options
        .connect(dir.path().join("patterns.db"), &[42; 32], true)
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE records(
            id INTEGER PRIMARY KEY, tenant_id INTEGER NOT NULL,
            value INTEGER NOT NULL, row_version INTEGER NOT NULL);
         CREATE TABLE audit(record_id INTEGER NOT NULL REFERENCES records(id));
         CREATE TABLE outbox(
            tenant_id INTEGER NOT NULL, request_key TEXT NOT NULL,
            record_id INTEGER NOT NULL REFERENCES records(id),
            UNIQUE(tenant_id, request_key));",
    )
    .execute(&pool)
    .await
    .unwrap();

    // Pool clones share physical connections; no service opens another database.
    let desktop_service = pool.clone();
    let host_service = pool.clone();
    assert!(apply(&desktop_service, "failed", true).await.is_err());
    assert_eq!(Record::count(&host_service).await.unwrap(), 0);
    let counts: (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM audit), (SELECT count(*) FROM outbox)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(counts, (0, 0));

    let (first, second) = tokio::join!(
        apply(&desktop_service, "same-request", false),
        apply(&host_service, "same-request", false)
    );
    let id = match (first, second) {
        (Ok(id), Err(error)) | (Err(error), Ok(id)) => {
            assert!(error.as_database_error().unwrap().is_unique_violation());
            id
        }
        _ => panic!("exactly one idempotent application must succeed"),
    };
    assert_eq!(Record::count(&pool).await.unwrap(), 1);
    let counts: (i64, i64) =
        sqlx::query_as("SELECT (SELECT count(*) FROM audit), (SELECT count(*) FROM outbox)")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(counts, (1, 1));

    let mut left = desktop_service.acquire().await.unwrap();
    let mut right = host_service.acquire().await.unwrap();
    assert!(matches!(
        pool.acquire().await,
        Err(sqlx::Error::PoolTimedOut)
    ));
    for connection in [&mut left, &mut right] {
        let settings: (i64, i64, String, i64) = sqlx::query_as(
            "SELECT (SELECT * FROM pragma_foreign_keys),
                    (SELECT * FROM pragma_busy_timeout),
                    (SELECT * FROM pragma_journal_mode),
                    (SELECT * FROM pragma_synchronous)",
        )
        .fetch_one(&mut **connection)
        .await
        .unwrap();
        assert_eq!(settings, (1, 200, "wal".into(), 2));
    }
    const UPDATE: &str = "UPDATE records SET value = ?, row_version = row_version + 1
                         WHERE tenant_id = ? AND id = ? AND row_version = ?";
    let (first, second) = tokio::join!(
        sqlx::query(UPDATE)
            .bind(20)
            .bind(1)
            .bind(id)
            .bind(0)
            .execute(&mut *left),
        sqlx::query(UPDATE)
            .bind(30)
            .bind(1)
            .bind(id)
            .bind(0)
            .execute(&mut *right)
    );
    let mut affected = [
        first.unwrap().rows_affected(),
        second.unwrap().rows_affected(),
    ];
    affected.sort();
    assert_eq!(affected, [0, 1]);
    // A mismatched tenant scope cannot update the row even with the current version.
    assert_eq!(
        sqlx::query(UPDATE)
            .bind(40)
            .bind(2)
            .bind(id)
            .bind(1)
            .execute(&mut *left)
            .await
            .unwrap()
            .rows_affected(),
        0
    );
    // A held writer causes a bounded busy failure; no library-level transaction retry.
    let mut tx = left.begin().await.unwrap();
    sqlx::query("UPDATE records SET value = value WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(
        sqlx::query(UPDATE)
            .bind(40)
            .bind(1)
            .bind(id)
            .bind(1)
            .execute(&mut *right)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    drop(left);
    drop(right);
    let row = Record::get(&pool, id).await.unwrap().unwrap();
    assert_eq!(row.row_version, 1);
    assert!([20, 30].contains(&row.value));
    pool.close().await;
    assert!(desktop_service.is_closed() && host_service.is_closed());
}

#[tokio::test]
async fn failed_migration_rolls_back_its_schema_and_reopens_encrypted() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("migration.db");
    let pool = flare_db::connect_encrypted_sqlite(&path, &[42; 32], true)
        .await
        .unwrap();
    let good = Migration::new(
        1,
        "kept".into(),
        MigrationType::Simple,
        "CREATE TABLE kept(id INTEGER PRIMARY KEY); INSERT INTO kept VALUES (1);".into_sql_str(),
        false,
    );
    let bad = Migration::new(
        2,
        "failed".into(),
        MigrationType::Simple,
        "CREATE TABLE rolled_back(id INTEGER); INSERT INTO no_such_table VALUES (1);"
            .into_sql_str(),
        false,
    );
    let migrator = Migrator::with_migrations(vec![good.clone(), bad]);
    assert!(flare_db::run_migrations(&pool, &migrator).await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM kept")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM sqlite_master WHERE name = 'rolled_back'"
        )
        .fetch_one(&pool)
        .await
        .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM _sqlx_migrations WHERE version = 2")
            .fetch_one(&pool)
            .await
            .unwrap(),
        0
    );
    pool.close().await;
    let pool = flare_db::connect_encrypted_sqlite(&path, &[42; 32], false)
        .await
        .unwrap();
    flare_db::run_migrations(&pool, &Migrator::with_migrations(vec![good]))
        .await
        .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM kept")
            .fetch_one(&pool)
            .await
            .unwrap(),
        1
    );
    pool.close().await;
}
