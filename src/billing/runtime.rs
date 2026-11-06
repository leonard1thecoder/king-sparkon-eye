//! Runtime resources for authenticated per-Eye-account billing (#245).
//!
//! Billing uses a dedicated native PostgreSQL DSN. It never reuses the OAuth PostgREST URL.
//! This runtime intentionally accepts test Stripe mode only until live billing is separately approved.

use std::str::FromStr;
use std::time::Duration;

use sqlx::{PgPool, postgres::{PgConnectOptions, PgPoolOptions, PgSslMode}};
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
        let return_origins = std::env::var(ENV_BILLING_RETURN_URL_ORIGINS).ok();
        let stripe_variables = [
            crate::billing::stripe::config::ENV_STRIPE_MODE,
            crate::billing::stripe::config::ENV_STRIPE_SECRET_KEY,
            crate::billing::stripe::config::ENV_STRIPE_API_BASE,
            crate::billing::stripe::config::ENV_STRIPE_PRICE_GOLD_MONTHLY,
            crate::billing::stripe::config::ENV_STRIPE_PRICE_GOLD_ANNUAL,
            crate::billing::stripe::config::ENV_STRIPE_PRICE_PREMIUM_MONTHLY,
            crate::billing::stripe::config::ENV_STRIPE_PRICE_PREMIUM_ANNUAL,
        ];
        let stripe_config_present = stripe_variables
            .iter()
            .any(|name| std::env::var_os(name).is_some());

        if database_url.is_none() && !stripe_config_present && return_origins.is_none() {
            return Ok(None);
        }

        let database_url = database_url
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
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

        let parsed_database_url = reqwest::Url::parse(&database_url)
            .map_err(|_| BillingRuntimeError::InvalidConfiguration)?;
        if !matches!(parsed_database_url.scheme(), "postgres" | "postgresql")
            || parsed_database_url.host_str().is_none()
        {
            return Err(BillingRuntimeError::InvalidConfiguration);
        }
        let local_database = matches!(
            parsed_database_url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]")
        );
        let connect_options = PgConnectOptions::from_str(&database_url)
            .map_err(|_| BillingRuntimeError::InvalidConfiguration)?
            .ssl_mode(if local_database {
                PgSslMode::Disable
            } else {
                PgSslMode::Require
            });
        let pool = PgPoolOptions::new()
            .max_connections(8)
            .acquire_timeout(Duration::from_secs(5))
            .connect_with(connect_options)
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
