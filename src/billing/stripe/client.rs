//! Minimal Stripe HTTP client and catalog verification (issue #139).
//!
//! Requests are bearer-authenticated, pinned to [`STRIPE_API_VERSION`], time-bounded, and never
//! follow redirects, so the secret key is never forwarded to another host. Errors map to safe
//! typed categories and never include Stripe's response body, which can contain customer data.

use std::time::Duration;

use serde::Deserialize;

use super::config::{PriceSlot, StripeMode, StripeSettings};
use crate::billing::BillingInterval;
use crate::config::SecretString;

/// The Stripe API version this client was written against. The Billing Meter reference was read
/// at this version on 2026-10-30. Upgrades are a deliberate change that must be re-verified.
pub const STRIPE_API_VERSION: &str = "2026-10-28.endive";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StripeError {
    #[error("Stripe is temporarily unavailable")]
    Unavailable,
    #[error("Stripe rejected the request with status {status}")]
    Rejected { status: u16 },
    #[error("Stripe returned a response that does not match the expected shape")]
    UnexpectedResponse,
    #[error("catalog price for {slot} failed verification: {reason}")]
    CatalogMismatch {
        slot: &'static str,
        reason: &'static str,
    },
}

impl StripeError {
    pub fn code(&self) -> &'static str {
        match self {
            StripeError::Unavailable => "STRIPE_UNAVAILABLE",
            StripeError::Rejected { .. } => "STRIPE_REJECTED",
            StripeError::UnexpectedResponse => "STRIPE_RESPONSE_UNEXPECTED",
            StripeError::CatalogMismatch { .. } => "STRIPE_CATALOG_MISMATCH",
        }
    }
}

/// The subset of a Stripe Price object that catalog verification reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PriceObject {
    pub id: String,
    pub active: bool,
    pub currency: String,
    pub livemode: bool,
    #[serde(default)]
    pub recurring: Option<Recurring>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Recurring {
    pub interval: String,
    #[serde(default)]
    pub usage_type: Option<String>,
}

/// Stripe client bound to validated settings.
pub struct StripeClient {
    http: reqwest::Client,
    base: String,
    secret: SecretString,
    mode: StripeMode,
}

impl StripeClient {
    pub fn new(settings: &StripeSettings) -> Result<Self, StripeError> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| StripeError::Unavailable)?;
        Ok(Self {
            http,
            base: settings.api_base.trim_end_matches('/').to_owned(),
            secret: settings.secret_key.clone(),
            mode: settings.mode,
        })
    }

    pub fn mode(&self) -> StripeMode {
        self.mode
    }

    /// Retrieves one price. The identifier is validated by configuration before it reaches here.
    pub async fn retrieve_price(&self, price_id: &str) -> Result<PriceObject, StripeError> {
        let url = format!("{}/v1/prices/{}", self.base, price_id);
        let response = self
            .http
            .get(url)
            .bearer_auth(self.secret.expose())
            .header("Stripe-Version", STRIPE_API_VERSION)
            .send()
            .await
            .map_err(|_| StripeError::Unavailable)?;
        let status = response.status();
        if !status.is_success() {
            return Err(map_status(status.as_u16()));
        }
        response
            .json::<PriceObject>()
            .await
            .map_err(|_| StripeError::UnexpectedResponse)
    }
}

fn map_status(status: u16) -> StripeError {
    if status == 429 || status >= 500 {
        StripeError::Unavailable
    } else {
        StripeError::Rejected { status }
    }
}

/// Verifies that one configured price is safe to use for its catalog slot.
///
/// The price must be active, USD, in the same livemode as the configured key, recurring at the
/// slot's interval, and licensed (not metered). Any failure names the slot and a fixed reason.
pub async fn verify_price_slot(
    client: &StripeClient,
    slot: PriceSlot,
    price_id: &str,
) -> Result<(), StripeError> {
    let price = client.retrieve_price(price_id).await?;
    let mismatch = |reason: &'static str| StripeError::CatalogMismatch {
        slot: slot.label(),
        reason,
    };

    if price.id != price_id {
        return Err(mismatch("returned identifier does not match the request"));
    }
    let live = client.mode() == StripeMode::Live;
    if price.livemode != live {
        return Err(mismatch("livemode does not match the configured mode"));
    }
    if !price.active {
        return Err(mismatch("price is inactive"));
    }
    if !price.currency.eq_ignore_ascii_case("usd") {
        return Err(mismatch("currency is not USD"));
    }
    let recurring = price
        .recurring
        .ok_or_else(|| mismatch("price is not recurring"))?;
    let expected_interval = match slot.interval {
        BillingInterval::Monthly => "month",
        BillingInterval::Annual => "year",
    };
    if recurring.interval != expected_interval {
        return Err(mismatch("interval does not match the slot"));
    }
    if recurring.usage_type.as_deref().unwrap_or("licensed") != "licensed" {
        return Err(mismatch("base price must be licensed, not metered"));
    }
    Ok(())
}

/// Verifies every configured price in the catalog. Stops at the first failure.
pub async fn verify_catalog(
    client: &StripeClient,
    settings: &StripeSettings,
) -> Result<(), StripeError> {
    for (slot, price_id) in settings.prices.iter() {
        verify_price_slot(client, *slot, price_id).await?;
    }
    Ok(())
}
