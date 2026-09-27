#![doc = include_str!("../README.md")]

mod backend;
#[cfg(feature = "sqlite")]
mod encrypted;
mod filter;
mod migrate;
mod page;

pub use backend::{Database, Pool, QUERY_BUILDER};
#[cfg(feature = "sqlite")]
pub use encrypted::{EncryptedSqliteOptions, connect_encrypted_sqlite};
pub use filter::FilterOp;
pub use migrate::run_migrations;
pub use page::Page;

pub use flare_db_macros::Crud;

// Re-exported so generated code (and consumers) don't need direct deps beyond
// `flare-db` + `sqlx` itself.
pub use sea_query;
pub use sea_query_binder;
pub use sqlx;
