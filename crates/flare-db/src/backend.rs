#[cfg(all(feature = "postgres", feature = "sqlite"))]
compile_error!(
    "flare-db: enable exactly one of the `postgres` or `sqlite` features, not both — a single \
     binary targets one backend at compile time (see the design doc's Substrate section)"
);

#[cfg(not(any(feature = "postgres", feature = "sqlite")))]
compile_error!("flare-db: enable one of the `postgres` or `sqlite` features");

/// Connection pool for the active backend, selected by the `postgres`/`sqlite` feature.
#[cfg(feature = "postgres")]
pub type Pool = sqlx::PgPool;
#[cfg(feature = "sqlite")]
pub type Pool = sqlx::SqlitePool;

/// The sea-query builder for the active backend — used by generated code to render
/// dialect-correct SQL (placeholder syntax, quoting) for either backend uniformly.
#[cfg(feature = "postgres")]
pub const QUERY_BUILDER: sea_query::PostgresQueryBuilder = sea_query::PostgresQueryBuilder;
#[cfg(feature = "sqlite")]
pub const QUERY_BUILDER: sea_query::SqliteQueryBuilder = sea_query::SqliteQueryBuilder;
