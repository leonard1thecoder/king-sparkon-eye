//! Eye PostgreSQL persistence for OAuth identity state (issue #112).
//!
//! Only the schema and the atomic operations live here. Wiring these into the
//! HTTP routes is part of issue #45 and is not done in this module.

pub mod enrollment_outbox;
pub mod identity;
pub mod oauth;

use sqlx::PgPool;

/// Embedded migrations from `migrations/`. Applied explicitly, never silently
/// at request time.
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// Open a pool to Eye's PostgreSQL database.
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPool::connect(database_url).await
}

/// Apply all pending migrations. Idempotent.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}
