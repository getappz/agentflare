use crate::Pool;

/// Runs a `sqlx::migrate!`-embedded [`sqlx::migrate::Migrator`] against `pool`.
///
/// Thin wrapper so callers don't need `sqlx::migrate` in scope directly:
///
/// ```ignore
/// static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");
/// flare_db::run_migrations(&pool, &MIGRATOR).await?;
/// ```
pub async fn run_migrations(
    pool: &Pool,
    migrator: &sqlx::migrate::Migrator,
) -> Result<(), sqlx::migrate::MigrateError> {
    migrator.run(pool).await
}
