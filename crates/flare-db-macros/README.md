# flare-db-macros

The `Crud` derive implementation for [flare-db](https://crates.io/crates/flare-db).
Use `flare-db` rather than depending on this crate directly. Generated code
references the `flare_db` crate and its selected SQLx backend, so keep the two
crate versions aligned. Requires Rust 1.94; licensed Apache-2.0.

Generated single-query methods accept SQLx executors, including a caller's
transaction connection. Batch updates use a transaction/savepoint and roll
back on failure. Runtime integration coverage lives in `flare-db/tests` and
is run for SQLite, SQLCipher and the PostgreSQL feature before publication.
