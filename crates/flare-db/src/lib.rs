//! Async, sqlx-backed repository derive macro for Postgres and SQLite.
//!
//! `#[derive(Crud)]` generates `list`/`get`/`create`/`update`/`delete`/`count` (plus
//! pagination, soft-delete, and a sea-query-based filtered path) per entity, closing
//! the same boilerplate gap MedusaJS's `MedusaService(models)` factory closes in TS —
//! without adopting a full ORM. See the attached design doc on item #287 for the full
//! rationale and the feature-adoption table.
//!
//! ## Deviation from the design doc: no `sqlx::query_as!` compile-time checking
//!
//! The design doc calls for the fixed-shape methods (`get`/`list`/`create_one`/...) to
//! be checked at compile time via the `sqlx::query_as!` macro, with only the dynamic
//! `_where` path going through sea-query without that check. In practice `query_as!`
//! needs a reachable `DATABASE_URL` (or a committed `.sqlx` offline cache prepared
//! against one) at *every consuming crate's* build time — table and column names are
//! only known once a consumer applies `#[derive(Crud)]` to their own struct, so that
//! obligation would fall on each downstream crate, not on `flare-db` itself. This
//! implementation instead builds every generated query (fixed-shape and filtered)
//! through sea-query and executes it with `sqlx::query_as_with`/`query_with` — dynamic,
//! not compile-time-checked, but backend-dialect-correct and buildable with no live
//! database required. The per-`FilterOp`-variant test coverage the design doc calls
//! for as "the honest reason this isn't a fully closed trade-off" now substitutes for
//! compile-time checking on *all* generated methods, not just the filtered ones.

mod backend;
mod filter;
mod migrate;
mod page;

pub use backend::{Pool, QUERY_BUILDER};
pub use filter::FilterOp;
pub use migrate::run_migrations;
pub use page::Page;

pub use flare_db_macros::Crud;

// Re-exported so generated code (and consumers) don't need direct deps beyond
// `flare-db` + `sqlx` itself.
pub use sea_query;
pub use sea_query_binder;
pub use sqlx;
