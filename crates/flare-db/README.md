# flare-db

Async CRUD derives for SQLx 0.9 and SQLite or PostgreSQL. Rust code: Apache-2.0;
vendored SQLCipher: BSD-3-Clause.
Requires Rust 1.94 or later. SQL is generated at runtime; entities must match
your migrations. This is not an ORM, an authorization layer, or a key vault.

## Installation

```toml
[dependencies]
flare-db = { version = "0.2.0", default-features = false, features = ["sqlcipher-bundled"] }
sqlx = { version = "0.9", default-features = false, features = ["derive"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
```

Select exactly one backend:

| Features | Storage |
| --- | --- |
| `sqlite` (default) | Ordinary SQLite; **not encrypted** |
| `sqlcipher-bundled` | Build and statically link vendored SQLCipher 4.19.0 and OpenSSL |
| `sqlcipher-bundled-external-openssl` | Build SQLCipher 4.19.0; dynamically link a prebuilt OpenSSL SDK (no Perl) |
| `sqlcipher-external` | Link an application-supplied SQLCipher 4.19+ SDK (major version 4) |
| `sqlcipher` | Backwards-compatible alias for `sqlcipher-external` |
| `postgres`, with defaults disabled | PostgreSQL; server encryption is the operator's responsibility |

Select only one SQLCipher mode. Combining modes is a build error.
`postgres` plus any SQLite mode, and no backend, are compile errors.
The SQLCipher helper is available under `sqlite` as well, but refuses to open
the target when the linked library lacks supported SQLCipher 4.19+. It never falls back to
plaintext. Enabling a feature alone does not encrypt ordinary `Pool::connect`
calls: encrypted consumers must use `connect_encrypted_sqlite` exclusively.

### Bundled native build

`sqlcipher-bundled` builds the pinned source in `vendor/sqlcipher` using `cc`
and OpenSSL from `openssl-src`. It uses `libsqlite3-sys` 0.37.0's bindings-only
mode (`in_gecko`) so SQLx does not build a second SQLite engine. Native source
is included in the Cargo package; the build script does not download an SDK.

On Windows, build for `x86_64-pc-windows-msvc` with Visual Studio C++ Build
Tools and a full Perl installation, such as Strawberry Perl. If Perl is not
on PATH, set `OPENSSL_SRC_PERL` to its `perl.exe`. Git for Windows' minimal
Perl is insufficient. On Linux, install a C compiler, make, and Perl.
These are **developer/CI requirements only**. The bundled build statically
links SQLCipher and OpenSSL; end users need neither SDK nor Perl. A Tauri
installer still needs its normal platform prerequisites and third-party notices.

```powershell
$env:OPENSSL_SRC_PERL = 'C:\Strawberry\perl\bin\perl.exe'
cargo build --release
```

Keep the application's Cargo.lock committed and update dependencies for native
security fixes. Include this crate's `NOTICE`, `vendor/sqlcipher/LICENSE.md`,
`vendor/sqlcipher/SQLITE_LICENSE.md`, and `vendor/OPENSSL-LICENSE.txt` with
distributed applications. This does not confer FIPS validation.

### External native SDK

To compile SQLCipher locally **without building OpenSSL or requiring Perl**,
select `sqlcipher-bundled-external-openssl`. This mode supports Windows MSVC
and Linux with a prebuilt OpenSSL 3 or 4 development SDK. It adds no Rust
OpenSSL dependency and does not download or discover an SDK automatically.
You still need a C compiler (MSVC C++ Build Tools on Windows).

```toml
flare-db = { version = "0.2.0", default-features = false, features = ["sqlcipher-bundled-external-openssl"] }
```

```powershell
$env:OPENSSL_DIR = "$env:USERPROFILE\scoop\apps\openssl\current"
$env:OPENSSL_INCLUDE_DIR = "$env:OPENSSL_DIR\include"
$env:OPENSSL_LIB_DIR = "$env:OPENSSL_DIR\lib"
$env:OPENSSL_STATIC = '0'
cargo build --release
```

Supply absolute paths. Explicit include/lib directories override `OPENSSL_DIR`;
otherwise its `include` and `lib` subdirectories are used. On Windows the library
directory must contain the matching architecture's **import** `libcrypto.lib`
(some installers use `lib\VC\x64\MD`). On Linux use the directory containing
`libcrypto.so` and set the runtime loader path if it is outside system locations.
This feature always requests dynamic crypto linkage; `OPENSSL_STATIC=1` is
rejected. For fully static crypto, use the original `sqlcipher-bundled` mode.

For a Windows installer, inspect the final executable with `dumpbin /DEPENDENTS`
and copy the **exact imported crypto DLL from the same SDK** beside that
executable (for the tested OpenSSL 4 x64 SDK, `libcrypto-4-x64.dll`). Do not rename
an OpenSSL 3 DLL to an OpenSSL 4 name or pick the first recursive match. If the
database lives in a Tauri sidecar, stage the DLL beside that sidecar as well;
putting it in an unrelated resources subdirectory is insufficient. A Tauri
resource mapping can place the explicitly selected DLL at the install root:

```json
"resources": { "binaries/libcrypto-4-x64.dll": "./" }
```

Verify the installed executable with a restricted PATH, including an encrypted
create/reopen operation. SQLCipher and `libssl` DLLs are not required by this
mode; the OpenSSL SDK's own runtime dependencies and Windows/MSVC runtime still
apply. Include the actual SDK's licenses/notices and keep it patched. SDKs,
headers, import libraries, C compilers and Perl are not end-user prerequisites.
The library does not edit application installer settings or copy DLLs for you.

### Fully external SQLCipher SDK

Use `features = ["sqlcipher-external"]` instead to maintain the SDK yourself.
That mode uses `libsqlite3-sys`'s external `sqlcipher` linkage, overriding
SQLx's ordinary bundled SQLite. The connector rejects versions before 4.19.0
and unknown/new major formats in either mode.

Build or obtain a vetted SQLCipher 4.19+ native SDK for your platform, following
the [upstream build instructions](https://github.com/sqlcipher/sqlcipher/tree/v4.19.0).
Enable `SQLITE_ENABLE_COLUMN_METADATA` and `SQLITE_ENABLE_UNLOCK_NOTIFY` in
that SDK for SQLx. SQLCipher 4.7+ defaults to the `libsqlite3` output name;
this binding's cipher feature expects `libsqlcipher`. In a private Linux SDK
prefix, link `libsqlcipher.so` to the built `libsqlite3.so` (as CI does).
Do not point this alias at the system's ordinary SQLite.
For a custom prefix set `SQLCIPHER_LIB_DIR` and `SQLCIPHER_INCLUDE_DIR`; on Unix
also point `PKG_CONFIG_PATH` at its `lib/pkgconfig` directory. Ship the matching
SQLCipher and crypto shared libraries with the app and configure its runtime
loader paths. On Windows use an MSVC-compatible `sqlcipher.lib` import library
and its DLLs. Static linking (`SQLCIPHER_STATIC=1`) also requires the native
crypto and platform dependencies to be linked. CI builds the pinned 4.19.0
source commit and tests against it. External mode does not build the vendored
cipher. The application must keep
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

The helper preflights the native cipher/version and sets the key on **every**
new connection before schema/page access. SQLx's initial foreign-key flag does
not access pages; cipher format and all page-dependent PRAGMAs run in the private
connection hook after keying. It selects SQLCipher 4 format with no plaintext
header, and validates a schema read before a connection can enter the pool.
It explicitly uses WAL, foreign-key enforcement, memory temporary storage, and
FULL synchronous durability on each physical connection. Defaults are five
connections and five-second acquisition/busy timeouts. `EncryptedSqliteOptions`
allows 1..=32 connections, acquisition waits in (0, 60s], and busy waits in
[0, 60s]; invalid settings fail before file access:

```rust,no_run
# #[cfg(feature = "sqlite")]
# async fn configured(path: &std::path::Path, key: &[u8]) -> flare_db::sqlx::Result<()> {
let pool = flare_db::EncryptedSqliteOptions {
    max_connections: 4,
    busy_timeout: std::time::Duration::from_secs(2),
    ..Default::default()
}.connect(path, key, false).await?;
let desktop_service_pool = pool.clone();
let host_service_pool = pool.clone(); // Same validated pool, no new database.
// Pass these typed SqlitePool handles to services; never let a framework reopen a URL.
pool.close().await; // Graceful shutdown waits for checked-out connections.
assert!(desktop_service_pool.is_closed() && host_service_pool.is_closed());
# Ok(())
# }
```

Wrong-key failures may surface as a database error or pool
timeout; neither path returns a usable pool.

Run your own `sqlx::migrate!` migrator using `run_migrations(&pool, &MIGRATOR)`
only after the helper succeeds. The packaged migrations are **test fixtures**,
not an application schema. Keep SQLx migration checksums intact.

Statement logging is disabled on these connections to avoid exposing the key.
`SqlitePool`, its connection/pool options, public tuning options, and connector
errors omit the raw key (covered by regression tests). The key is retained in
a private callback for replacement connections, outside debug-visible options,
and its initialization statement is not cached. Paths may still appear in
connection options; treat filesystem paths as application metadata.
Connection options alone intentionally contain no key: share/clone the returned
pool, never reopen a connection from those options or let a framework rebuild it.
This API does not promise
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

Use bound SQLx queries on the same transaction for audit/outbox records.
For optimistic writes, bind tenant scope, primary key, and expected version in
`WHERE tenant_id = ? AND id = ? AND row_version = ?`, increment the version,
and inspect `execute(...).await?.rows_affected()`: zero means no matching
version/scope. Enforce request idempotency with a database UNIQUE constraint
inside that transaction. Concurrent integration checks demonstrate one winner
and no partial audit/outbox writes. This crate does not retry business
transactions or deliver outbox events; callers decide how to handle contention.

SQLx migrations are atomic **per migration**, not across an entire batch.
If a transactional migration fails, its schema/data and tracking entry roll
back; previously committed migrations remain applied. Stop application startup
on any migration error. Migrations explicitly marked `no_tx` do not offer
this rollback guarantee. Encrypted failure/reopen behaviour is tested.

`list_and_count` uses the caller/backend isolation level. PostgreSQL's default
READ COMMITTED is not a repeatable snapshot; use a caller transaction with
REPEATABLE READ when that guarantee is needed. Filtered reads exclude
soft-deleted rows unless `with_deleted` is set or `deleted_at` is filtered
explicitly (a behavior change from earlier versions, which included them), and
an empty filter on a bulk write affects **all rows**. These are query primitives, not
tenant or business authorization checks. Apply tenant filters/constraints and
check permissions in the consuming application.

## Model-controlled data access (field policies and friends)

The entity definition decides what goes in and what comes out, enforced by
generated types rather than runtime checks. Still no relations, populate,
cascades or DDL: this is a data-access layer, not an ORM.

```rust,no_run
use flare_db::{Crud, sqlx};
use time::OffsetDateTime;

#[derive(sqlx::FromRow, Crud)]
#[crud(table = "orders", pk = "id", soft_delete, unique(email))]
struct Order {
    #[crud(id(prefix = "ord"))] // New.id: Option<String>; None => "ord_<ULID>"
    id: String,
    #[crud(immutable, searchable)] // in New; never in Patch or upsert updates
    email: String,
    #[crud(enum_values("open", "paid"), default = "open".to_string())]
    status: String, // New.status: Option<String>; choices checked before writes
    #[crud(readonly)] // readable; in neither New nor Patch
    version: i64,
    #[crud(hidden)] // insertable/patchable, never selected or returned
    api_secret: String,
    #[crud(computed)] // not a column; `Default` in output
    label: String,
    #[crud(created_at)] // set on insert
    created_at: OffsetDateTime,
    #[crud(updated_at)] // set on insert and every update
    updated_at: OffsetDateTime,
    deleted_at: Option<OffsetDateTime>,
}
```

| Field attribute | New | Patch | Returned |
| --- | --- | --- | --- |
| `readonly` | no | no | yes |
| `immutable` | yes | no (never upserted either) | yes |
| `hidden` | yes | yes | **no** (`OrderPublic` lacks it) |
| `computed` | no | no | `Default` (no SQL at all) |
| `created_at` / `updated_at` | no (auto) | no (auto bump) | yes |
| `id(prefix = "..")` / `default = ..` | `Option<T>` | no / yes | yes |

- **Output type.** Every read and write return path selects `Entity::COLUMNS`
  and decodes into one type: the entity itself, or `{Entity}Public` when any
  field is `hidden`/`computed`. Hidden columns are never in a SELECT/RETURNING
  list; filters may still reference them (e.g. look up by token hash), but
  `searchable` ignores them. Generated output types carry no derives.
- **Projection.** `{Entity}Field` names the selectable columns;
  `get_select(pool, id, &[Field::Id])` and `list_select(pool, filter, page, &[..])`
  return `{Entity}Partial` (every field an `Option`; unselected = `None`).
- **Errors.** Methods still return `sqlx::Result` (no breaking change);
  `CrudError::from(err)` classifies it as `NotFound`, `Conflict { table, cols,
  values }` (values on Postgres only), `NotNull`, `ForeignKey`, `Validation` or
  `Other`, with readable `Display`. Validation failures travel in
  `sqlx::Error::Encode`.
- **Validation.** `enum_values(..)` is always checked; `#[crud(validate)]` also
  calls your `flare_db::Validate` impls on `{Entity}New`/`{Entity}Patch` before
  insert, update and upsert.
- **Events.** `flare_db::set_event_sink(|e| ..)` receives `Created`, `Updated`,
  `Deleted`, `Restored` or `Upserted` with `{ object, id }` after each successful
  statement (soft delete => `Deleted`, restore => `Restored`; `*_where` bulk
  writes carry `id: None`). Inside a caller transaction that means "statement
  succeeded", not "committed": use an outbox for commit-gated delivery.
  `update_many` emits after its own commit.
- **Upsert.** `upsert_one`/`upsert_many` (target from `unique(..)`) and
  `upsert_one_on`/`upsert_many_on(pool, &[Field], ..)` use `ON CONFLICT (..) DO
  UPDATE`. Only patchable columns are overwritten (never immutable, readonly, pk
  or `created_at`); `default` fields are overwritten only for rows where the caller
  supplied a value (`Some`), so an unset default never clobbers the stored one.
  `updated_at` is bumped. Rows are grouped by which defaults they supply (one
  statement per group, one transaction), so results follow group order. The target
  must match a UNIQUE index; duplicate keys within one batch are a database error.
- **Search and filters.** `{Entity}Filter.q` (present when a field is
  `searchable`) ORs a case-insensitive substring match over those fields, with
  `%`, `_` and `\` matched literally. `FilterOp` adds `Gt`, `Gte`, `Lt`, `Lte`,
  `Between` and `ILike`; `and`/`or` hold nested filters; `count_where` and
  `list_and_count_where` take a filter. Case folding is `ILIKE` on Postgres and
  ASCII-only on SQLite.
- **Timestamps.** `created_at`/`updated_at` must be `OffsetDateTime` (or
  `Option<..>`; checked at compile time) and are bound from Rust. `updated_at` is
  bumped by every update, including `soft_delete`/`restore` and their `_where`
  forms.
- **Output shaping is type-level.** There is no serialization layer, so a Medusa-style
  "skip null" option does not exist: nullable columns keep their `Option` values.

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
