//! Minimal Stripe HTTP client and catalog verification (issue #139).
//!
//! Requests are bearer-authenticated, pinned to [`STRIPE_API_VERSION`], time-bounded, and never
//! follow redirects, so the secret key is never forwarded to another host. Errors map to safe
//! typed categories and never include Stripe's response body, which can contain customer data.

use std::time::Duration;

use reqwest::Url;
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
    #[error("Stripe request parameters failed local validation: {reason}")]
    InvalidRequest { reason: &'static str },
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
            StripeError::InvalidRequest { .. } => "STRIPE_REQUEST_INVALID",
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

/// Exact-origin allowlist for hosted billing return URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReturnUrlPolicy {
    origins: std::collections::BTreeSet<String>,
}

impl ReturnUrlPolicy {
    pub fn new(origins: impl IntoIterator<Item = String>) -> Result<Self, StripeError> {
        let mut allowed = std::collections::BTreeSet::new();
        for value in origins {
            let url = Url::parse(&value).map_err(|_| StripeError::InvalidRequest {
                reason: "return origin is invalid",
            })?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(StripeError::InvalidRequest {
                    reason: "return origin must be a bare HTTPS origin",
                });
            }
            allowed.insert(url.origin().ascii_serialization());
        }
        if allowed.is_empty() {
            return Err(StripeError::InvalidRequest {
                reason: "at least one return origin is required",
            });
        }
        Ok(Self { origins: allowed })
    }

    pub fn validate(&self, value: &str) -> Result<String, StripeError> {
        let url = Url::parse(value).map_err(|_| StripeError::InvalidRequest {
            reason: "return URL is invalid",
        })?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
            || !self.origins.contains(&url.origin().ascii_serialization())
        {
            return Err(StripeError::InvalidRequest {
                reason: "return URL origin is not allowed",
            });
        }
        // Preserve Stripe's required literal `{CHECKOUT_SESSION_ID}` success-URL token.
        Ok(value.to_owned())
    }
}

/// Hosted Stripe URL. No Debug implementation: short-lived customer URLs must not be logged.
pub struct HostedSession {
    pub id: String,
    pub url: String,
}

#[derive(Deserialize)]
struct HostedSessionResponse {
    id: String,
    url: Option<String>,
}

fn valid_stripe_id(value: &str, prefix: &str) -> bool {
    value.len() > prefix.len()
        && value.len() <= 255
        && value.starts_with(prefix)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn valid_idempotency_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
}

/// Stripe client bound to validated settings.
pub struct StripeClient {
    http: reqwest::Client,
    base: String,
    secret: SecretString,
    mode: StripeMode,
    prices: super::config::StripePriceMap,
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
            prices: settings.prices.clone(),
        })
    }

    pub fn mode(&self) -> StripeMode {
        self.mode
    }

    /// Creates a subscription Checkout Session for an existing, server-mapped Stripe Customer.
    /// Price selection is exclusively from validated server configuration; callers must not
    /// pass browser-provided customer IDs, Price IDs, amounts, currencies or return URLs.
    pub async fn create_checkout_session(
        &self,
        slot: PriceSlot,
        customer_id: &str,
        success_url: &str,
        cancel_url: &str,
        return_urls: &ReturnUrlPolicy,
        idempotency_key: &str,
    ) -> Result<HostedSession, StripeError> {
        if self.mode != StripeMode::Test {
            return Err(StripeError::InvalidRequest {
                reason: "live billing is disabled",
            });
        }
        if !valid_stripe_id(customer_id, "cus_") {
            return Err(StripeError::InvalidRequest {
                reason: "customer identifier is invalid",
            });
        }
        if !valid_idempotency_key(idempotency_key) {
            return Err(StripeError::InvalidRequest {
                reason: "idempotency key is invalid",
            });
        }
        let price_id = self.prices.get(slot).ok_or(StripeError::InvalidRequest {
            reason: "configured price is missing",
        })?;
        let success = return_urls.validate(success_url)?;
        let cancel = return_urls.validate(cancel_url)?;
        let response = self
            .http
            .post(format!("{}/v1/checkout/sessions", self.base))
            .bearer_auth(self.secret.expose())
            .header("Stripe-Version", STRIPE_API_VERSION)
            .header("Idempotency-Key", idempotency_key)
            .form(&[
                ("mode", "subscription"),
                ("customer", customer_id),
                ("line_items[0][price]", price_id),
                ("line_items[0][quantity]", "1"),
                ("success_url", success.as_str()),
                ("cancel_url", cancel.as_str()),
            ])
            .send()
            .await
            .map_err(|_| StripeError::Unavailable)?;
        parse_hosted_session(response, "checkout.stripe.com").await
    }

    /// Creates a Customer Portal session for an existing, server-mapped Stripe Customer.
    pub async fn create_portal_session(
        &self,
        customer_id: &str,
        return_url: &str,
        return_urls: &ReturnUrlPolicy,
        idempotency_key: &str,
    ) -> Result<HostedSession, StripeError> {
        if self.mode != StripeMode::Test {
            return Err(StripeError::InvalidRequest {
                reason: "live billing is disabled",
            });
        }
        if !valid_stripe_id(customer_id, "cus_") {
            return Err(StripeError::InvalidRequest {
                reason: "customer identifier is invalid",
            });
        }
        if !valid_idempotency_key(idempotency_key) {
            return Err(StripeError::InvalidRequest {
                reason: "idempotency key is invalid",
            });
        }
        let return_url = return_urls.validate(return_url)?;
        let response = self
            .http
            .post(format!("{}/v1/billing_portal/sessions", self.base))
            .bearer_auth(self.secret.expose())
            .header("Stripe-Version", STRIPE_API_VERSION)
            .header("Idempotency-Key", idempotency_key)
            .form(&[
                ("customer", customer_id),
                ("return_url", return_url.as_str()),
            ])
            .send()
            .await
            .map_err(|_| StripeError::Unavailable)?;
        parse_hosted_session(response, "billing.stripe.com").await
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

async fn parse_hosted_session(
    response: reqwest::Response,
    expected_host: &str,
) -> Result<HostedSession, StripeError> {
    let status = response.status();
    if !status.is_success() {
        return Err(map_status(status.as_u16()));
    }
    let value = response
        .json::<HostedSessionResponse>()
        .await
        .map_err(|_| StripeError::UnexpectedResponse)?;
    let url = value.url.ok_or(StripeError::UnexpectedResponse)?;
    let parsed = Url::parse(&url).map_err(|_| StripeError::UnexpectedResponse)?;
    if parsed.scheme() != "https"
        || parsed.host_str() != Some(expected_host)
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || !valid_stripe_id(&value.id, "cs_")
    {
        return Err(StripeError::UnexpectedResponse);
    }
    Ok(HostedSession { id: value.id, url })
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

#[cfg(test)]
mod return_url_tests {
    use super::{ReturnUrlPolicy, StripeError, valid_idempotency_key, valid_stripe_id};

    fn policy() -> ReturnUrlPolicy {
        ReturnUrlPolicy::new(["https://app.example.com/".to_owned()]).expect("valid origin")
    }

    #[test]
    fn allows_paths_and_queries_on_an_exact_configured_https_origin() {
        let value = policy()
            .validate("https://app.example.com/billing/return?source=portal")
            .expect("allowed");
        assert_eq!(
            value,
            "https://app.example.com/billing/return?source=portal"
        );
    }

    #[test]
    fn rejects_unlisted_origins_and_subdomain_confusion() {
        for value in [
            "https://evil.example.com/return",
            "https://app.example.com.evil.test/",
            "http://app.example.com/",
        ] {
            assert!(
                matches!(
                    policy().validate(value),
                    Err(StripeError::InvalidRequest { .. })
                ),
                "{value}"
            );
        }
    }

    #[test]
    fn rejects_credentials_fragments_and_non_origin_allowlist_entries() {
        for value in [
            "https://user@app.example.com/",
            "https://app.example.com/#fragment",
        ] {
            assert!(policy().validate(value).is_err(), "{value}");
        }
        for origin in [
            "http://app.example.com/",
            "https://app.example.com/path",
            "https://user@app.example.com/",
        ] {
            assert!(
                ReturnUrlPolicy::new([origin.to_owned()]).is_err(),
                "{origin}"
            );
        }
        assert!(ReturnUrlPolicy::new(Vec::<String>::new()).is_err());
    }

    #[test]
    fn customer_and_idempotency_identifiers_are_strictly_validated() {
        assert!(valid_stripe_id("cus_123abc", "cus_"));
        assert!(!valid_stripe_id("cus_", "cus_"));
        assert!(!valid_stripe_id("cus_123/../x", "cus_"));
        assert!(valid_idempotency_key("tenant_42_checkout_gold_monthly"));
        assert!(!valid_idempotency_key("contains spaces"));
        assert!(!valid_idempotency_key(""));
    }
    #[tokio::test]
    async fn live_mode_cannot_create_checkout_or_portal_sessions_even_if_configured() {
        use super::super::config::{
            ENV_STRIPE_MODE, ENV_STRIPE_PRICE_GOLD_ANNUAL, ENV_STRIPE_PRICE_GOLD_MONTHLY,
            ENV_STRIPE_PRICE_PREMIUM_ANNUAL, ENV_STRIPE_PRICE_PREMIUM_MONTHLY,
            ENV_STRIPE_SECRET_KEY,
        };
        use super::super::{PriceSlot, StripeClient, StripeSettings};
        use crate::billing::{BillingInterval, PlanId};
        use std::collections::HashMap;

        let env = HashMap::from([
            (ENV_STRIPE_MODE, "live".to_owned()),
            (
                ENV_STRIPE_SECRET_KEY,
                "sk_live_placeholder_not_a_real_key".to_owned(),
            ),
            (
                ENV_STRIPE_PRICE_GOLD_MONTHLY,
                "price_gold_monthly".to_owned(),
            ),
            (ENV_STRIPE_PRICE_GOLD_ANNUAL, "price_gold_annual".to_owned()),
            (
                ENV_STRIPE_PRICE_PREMIUM_MONTHLY,
                "price_premium_monthly".to_owned(),
            ),
            (
                ENV_STRIPE_PRICE_PREMIUM_ANNUAL,
                "price_premium_annual".to_owned(),
            ),
        ]);
        let settings = StripeSettings::from_lookup(|name| env.get(name).cloned(), true)
            .expect("valid settings shape");
        let client = StripeClient::new(&settings).expect("client");
        let urls = policy();
        let checkout = client
            .create_checkout_session(
                PriceSlot {
                    plan: PlanId::Gold,
                    interval: BillingInterval::Monthly,
                },
                "cus_123abc",
                "https://app.example.com/success",
                "https://app.example.com/cancel",
                &urls,
                "checkout_once",
            )
            .await;
        let portal = client
            .create_portal_session(
                "cus_123abc",
                "https://app.example.com/billing",
                &urls,
                "portal_once",
            )
            .await;
        assert!(matches!(
            checkout,
            Err(StripeError::InvalidRequest {
                reason: "live billing is disabled",
            })
        ));
        assert!(matches!(
            portal,
            Err(StripeError::InvalidRequest { reason: "live billing is disabled" })
        ));
    }

}
