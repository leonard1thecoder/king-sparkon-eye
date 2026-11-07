//! Eye PostgreSQL persistence for OAuth identity state (issue #112).
//!
//! Only the schema and the atomic operations live here. Wiring these into the
//! HTTP routes is part of issue #45 and is not done in this module.

pub mod billing_accounts;
pub mod enrollment_outbox;
pub mod identity;
pub mod oauth;
pub mod usage_ledger;

use sqlx::{PgPool, postgres::PgPoolOptions};

const EYE_SEARCH_PATH: &str = "SET search_path TO eye, public";

/// Open a pool whose unqualified SQL is confined to Eye's schema.
///
/// The public schema is intentionally retained as a read-only fallback for shared
/// PostgreSQL installations; Eye migrations and writes resolve in the first schema.
pub async fn connect(database_url: &str) -> Result<PgPool, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .after_connect(|connection, _metadata| {
            Box::pin(async move {
                sqlx::query(EYE_SEARCH_PATH)
                    .execute(&mut *connection)
                    .await?;
                Ok(())
            })
        })
        .connect(database_url)
        .await?;

    sqlx::query("CREATE SCHEMA IF NOT EXISTS eye")
        .execute(&pool)
        .await?;

    Ok(pool)
}

/// Create a lazy Eye-scoped pool after startup migration has initialized the schema.
pub fn connect_lazy(database_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .after_connect(|connection, _metadata| {
            Box::pin(async move {
                sqlx::query(EYE_SEARCH_PATH)
                    .execute(&mut *connection)
                    .await?;
                Ok(())
            })
        })
        .connect_lazy(database_url)
}

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
