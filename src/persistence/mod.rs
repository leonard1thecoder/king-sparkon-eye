//! Eye PostgreSQL persistence for OAuth identity state (issue #112).
//!
//! Only the schema and the atomic operations live here. Wiring these into the
//! HTTP routes is part of issue #45 and is not done in this module.

pub mod api_credentials;
pub mod billing_accounts;
pub mod enrollment_outbox;
pub mod identity;
pub mod identity_registry;
pub mod jobs;
pub mod oauth;
pub mod privacy;
pub mod tenants;
pub mod verification;
pub mod usage_ledger;

use sqlx::{PgPool, postgres::PgPoolOptions};

const EYE_SEARCH_PATH: &str = "SET search_path TO eye";

/// Open a pool whose unqualified SQL is confined to Eye's schema.
///
/// No public-schema fallback is configured: a missing Eye table must fail closed rather
/// than accidentally resolving to a similarly named table owned by another application.
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
pub static MIGRATOR: std::sync::LazyLock<sqlx::migrate::Migrator> =
    std::sync::LazyLock::new(|| {
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.dangerous_set_table_name("eye._sqlx_migrations");
        migrator.create_schema("eye");
        migrator
    });

/// Apply all pending migrations. Idempotent.
pub async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    MIGRATOR.run(pool).await
}
