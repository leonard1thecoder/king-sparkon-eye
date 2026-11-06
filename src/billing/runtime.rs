//! Runtime resources for authenticated per-Eye-account billing (#245).
//!
//! Billing uses a dedicated native PostgreSQL DSN. It never reuses the OAuth PostgREST URL.
//! This runtime intentionally accepts test Stripe mode only until live billing is separately approved.

use std::time::Duration;

use sqlx::{PgPool, postgres::PgPoolOptions};
use thiserror::Error;

use crate::billing::{
    stripe::{ReturnUrlPolicy, StripeClient, StripeMode, StripeSettings},
};
use crate::persistence::{self, billing_accounts::BillingAccountStore};

pub const ENV_BILLING_DATABASE_URL: &str = "EYE_BILLING_DATABASE_URL";
pub const ENV_BILLING_RETURN_URL_ORIGINS: &str = "EYE_BILLING_RETURN_URL_ORIGINS";

#[derive(Debug, Error)]
pub enum BillingRuntimeError {
    #[error("billing runtime configuration is incomplete or invalid")]
    InvalidConfiguration,
    #[error("billing PostgreSQL connection failed")]
    DatabaseUnavailable,
    #[error("billing database migrations failed")]
    MigrationFailed,
    #[error("billing Stripe client could not be initialized")]
    StripeUnavailable,
}

pub struct BillingRuntime {
    pool: PgPool,
    pub store: BillingAccountStore,
    pub stripe: StripeClient,
    pub return_urls: ReturnUrlPolicy,
}

impl BillingRuntime {
    /// Billing is disabled when none of its environment variables are present. Partial
    /// configuration is a startup error rather than a silently disabled billing surface.
    pub async fn from_env() -> Result<Option<Self>, BillingRuntimeError> {
        let database_url = std::env::var(ENV_BILLING_DATABASE_URL).ok();
        let stripe_mode = std::env::var(crate::billing::stripe::config::ENV_STRIPE_MODE).ok();
        let return_origins = std::env::var(ENV_BILLING_RETURN_URL_ORIGINS).ok();

        if database_url.is_none() && stripe_mode.is_none() && return_origins.is_none() {
            return Ok(None);
        }

        let database_url = database_url
            .filter(|value| !value.trim().is_empty())
            .ok_or(BillingRuntimeError::InvalidConfiguration)?;
        let return_origins = return_origins
            .filter(|value| !value.trim().is_empty())
            .ok_or(BillingRuntimeError::InvalidConfiguration)?;
        let origins = return_origins
            .split(',')
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        if origins.is_empty() {
            return Err(BillingRuntimeError::InvalidConfiguration);
        }
        let return_urls = ReturnUrlPolicy::new(origins)
            .map_err(|_| BillingRuntimeError::InvalidConfiguration)?;

        let stripe_settings = StripeSettings::from_lookup(
            |name| std::env::var(name).ok(),
            false,
        )
        .map_err(|_| BillingRuntimeError::InvalidConfiguration)?;
        if stripe_settings.mode != StripeMode::Test {
            return Err(BillingRuntimeError::InvalidConfiguration);
        }
        let stripe = StripeClient::new(&stripe_settings)
            .map_err(|_| BillingRuntimeError::StripeUnavailable)?;

        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .connect(&database_url)
            .await
            .map_err(|_| BillingRuntimeError::DatabaseUnavailable)?;
        persistence::migrate(&pool)
            .await
            .map_err(|_| BillingRuntimeError::MigrationFailed)?;

        let store = BillingAccountStore::new(pool.clone());
        Ok(Some(Self {
            pool,
            store,
            stripe,
            return_urls,
        }))
    }

    /// Explicitly close the pool after the HTTP server has stopped accepting requests.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}
