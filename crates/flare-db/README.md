# flare-db

Async CRUD derives for SQLx 0.9 and SQLite or PostgreSQL. Apache-2.0.
Requires Rust 1.94 or later. SQL is generated at runtime; entities must match
your migrations. This is not an ORM, an authorization layer, or a key vault.

## Installation

```toml
[dependencies]
flare-db = { version = "0.1.0", default-features = false, features = ["sqlcipher"] }
sqlx = { version = "0.9", default-features = false, features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

Select exactly one backend:

| Features | Storage |
| --- | --- |
| `sqlite` (default) | Ordinary SQLite; **not encrypted** |
| `sqlcipher` | External SQLCipher 4.19+ (major version 4), maintained by the application |
| `postgres`, with defaults disabled | PostgreSQL; server encryption is the operator's responsibility |

`postgres` plus `sqlite`/`sqlcipher`, and no backend, are compile errors.
The SQLCipher helper is available under `sqlite` as well, but refuses to open
the target when the linked library lacks supported SQLCipher 4.19+. It never falls back to
plaintext. Enabling a feature alone does not encrypt ordinary `Pool::connect`
calls: encrypted consumers must use `connect_encrypted_sqlite` exclusively.

SQLx 0.9 supports native bindings below 0.38; this crate selects
`libsqlite3-sys` 0.37 with its **external** `sqlcipher` feature. That feature
overrides SQLx's ordinary bundled SQLite. Older bundled SQLCipher versions are
not an acceptable production default: the connector rejects versions before
4.19.0 and unknown/new major formats. There is no vendored cipher fallback.

Build or obtain a vetted SQLCipher 4.19+ native SDK for your platform, following
the [upstream build instructions](https://github.com/sqlcipher/sqlcipher/tree/v4.19.0).
For a custom prefix set `SQLCIPHER_LIB_DIR` and `SQLCIPHER_INCLUDE_DIR`; on Unix
also point `PKG_CONFIG_PATH` at its `lib/pkgconfig` directory. Ship the matching
SQLCipher and crypto shared libraries with the app and configure its runtime
loader paths. On Windows use an MSVC-compatible `sqlcipher.lib` import library
and its DLLs. Static linking (`SQLCIPHER_STATIC=1`) also requires the native
crypto and platform dependencies to be linked. CI builds the pinned 4.19.0
source commit and tests against it; the crate archive does not contain native
code or download/build a cipher for the consumer. The application must keep
its native SDK patched; check `PRAGMA cipher_version` during release testing.

Cargo allows one native `sqlite3` provider per binary. Other SQLite users
(including a Tauri SQL plugin or rusqlite) must resolve the same compatible
`libsqlite3-sys` version and cipher features. Plugins still pinned to SQLx 0.8
and native bindings 0.30 cannot be combined with these 0.37 bindings; upgrade
or remove the conflicting plugin. Keep a direct SQLx 0.9 dependency for the
`FromRow` derive, which generates absolute SQLx paths. Do not add a second
SQLite binding or a plaintext connection to work around a dependency conflict.

## Encrypted connections and migrations

```rust,no_run
# #[cfg(feature = "sqlite")]
# mod example {
use flare_db::{connect_encrypted_sqlite, sqlx};

async fn open_database(path: &std::path::Path, key_from_keychain: &[u8])
    -> sqlx::Result<flare_db::Pool>
{
    // false for normal opens: a missing database is an error, not a blank replacement.
    connect_encrypted_sqlite(path, key_from_keychain, false).await
}
# }
```

Provision a cryptographically random 32-byte key in the application's OS
keychain first. Only first-time setup should pass `create_if_missing = true`.
Do not derive a raw key by padding/truncating a password. Missing or incorrectly
sized keys fail before opening the file. Wrong keys and existing plaintext
databases are rejected; they are not overwritten or converted.

The helper preflights the native cipher/version, sets the key before SQLx's other
PRAGMAs on **every** new connection, selects SQLCipher 4 format with no plaintext
header, and validates a schema read before a connection can enter the pool.
It uses WAL, foreign-key enforcement, memory temporary storage, and SQLx's
FULL synchronous default. The pool allows five connections with a five-second
acquisition timeout. Wrong-key failures may surface as a database error or pool
timeout; neither path returns a usable pool.

Run your own `sqlx::migrate!` migrator using `run_migrations(&pool, &MIGRATOR)`
only after the helper succeeds. The packaged migrations are **test fixtures**,
not an application schema. Keep SQLx migration checksums intact.

Statement logging is disabled on these connections to avoid exposing the key.
**Never debug-print `pool.connect_options()`**: SQLx stores the key in its
connection options to open replacement connections. This API does not promise
key-memory zeroization, protection from an already-compromised process, or
encryption of exports/logs. The app owns access controls, OS-keychain lifetime,
file permissions, disk/swap protection, and redaction of business data.

## CRUD and caller-controlled transactions

```rust,no_run
use flare_db::{Crud, Pool, sqlx};

#[derive(Debug, sqlx::FromRow, Crud)]
#[crud(table = "invoices", pk = "id")]
struct Invoice { id: i64, amount_minor: i64 }

async fn invoice_and_audit(pool: &Pool) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    let invoice = Invoice::create_one(&mut *tx, InvoiceNew { amount_minor: 12500 }).await?;
    sqlx::query("INSERT INTO audit_log (invoice_id) VALUES ($1)")
        .bind(invoice.id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}
```

The derive generates `EntityNew`, `EntityPatch`, and `EntityFilter`, plus:
`get`, `list`, `count`, `list_and_count`, `create_one`, `create_many`,
`update_one`, `update_many`, `list_where`, `update_where`, and `delete`.
`#[crud(soft_delete)]` requires `deleted_at: Option<T>` and generates
`soft_delete`, `restore`, `soft_delete_where`, and `restore_where` instead of
hard delete. Primary keys are database-generated; use migrations to define
identity/autoincrement and constraints. Patch fields are `Option<T>`;
nullable fields become `Option<Option<T>>` so `Some(None)` writes SQL NULL.

Single-query methods accept SQLx `Executor`; pass `&pool`, `&mut connection`,
or `&mut *tx`. Multi-query `update_many` and `list_and_count` accept `Acquire`
and start a transaction/savepoint. Batch update failures roll back the whole
batch; committing a nested savepoint never commits the caller's transaction.
On error, propagate it or roll back explicitly; dropping a SQLx transaction
also schedules rollback. Do not catch an error and then commit a business
transaction that should have been cancelled.

`list_and_count` uses the caller/backend isolation level. PostgreSQL's default
READ COMMITTED is not a repeatable snapshot; use a caller transaction with
REPEATABLE READ when that guarantee is needed. Filtered reads intentionally
include soft-deleted rows unless your filter excludes them, and an empty
filter on a bulk write affects **all rows**. These are query primitives, not
tenant or business authorization checks. Apply tenant filters/constraints and
check permissions in the consuming application.

## Backup, restore, key rotation and plaintext migration

Supported backup boundary: **offline encrypted file copy only**. Stop every
writer/reader in every process, release handles, checkpoint WAL successfully,
and await `pool.close()`. Verify no uncheckpointed WAL remains before copying
the database to a new backup file. Never copy only the main file while it is
open, and never remove WAL/journal files to force a backup. Preserve the key
separately in secure recovery storage. Restore to a new path with all users
closed; open it with the correct key and `create_if_missing = false`, verify
schema/data and migration state, then switch the application to it. A copied
file remains encrypted and the wrong key is rejected (covered by tests).

Online backup, automatic key rotation/rekey, SQLCipher legacy-format upgrades,
and automatic plaintext migration are **not provided** in this release.
For a future plaintext import, use a separately reviewed offline tool and
SQLCipher's `sqlcipher_export` into a **new keyed database**; verify schema,
indexes, row counts, migration metadata and application integrity before
cutover. Plaintext sources, backups and disk remnants remain sensitive.
`PRAGMA key`/`rekey` alone is not a plaintext migration. This library never
runs migration/export commands against a user's existing database.

References: [SQLCipher keying and API](https://www.zetetic.net/sqlcipher/sqlcipher-api/),
[plaintext conversion](https://www.zetetic.net/sqlcipher/encrypting-plaintext-databases/),
[SQLx executor usage](https://docs.rs/sqlx/0.9.0/sqlx/trait.Executor.html).

## Release checks

From this crate's directory:

```text
cargo test
cargo test --features sqlcipher
cargo test --no-default-features --features postgres
cargo test --no-default-features --features integration-postgres --test postgres_integration
cargo package --list
cargo publish --dry-run
```

The last integration command needs a disposable PostgreSQL `DATABASE_URL`;
it creates and drops its test table. PostgreSQL feature tests without that
feature compile generated queries but do not contact a server.

Publish `flare-db-macros` first, then `flare-db` after its registry dependency
is visible. Inspect both package archives, retain LICENSE/NOTICE and native
dependency licenses in downstream distributions. See NOTICE for scope.
