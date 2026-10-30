//! Stripe settings with strict test/live separation (issue #139).
//!
//! Test and live configurations never share a key, a price identifier, or a mode. Live mode is
//! refused unless the billing catalog has been approved, and the catalog is currently
//! `candidate_unapproved`, so live settings cannot be loaded yet. Secrets are never printed:
//! `SecretString` redacts its `Debug` output, and errors name the variable but never echo a value.

use std::collections::BTreeMap;

use crate::billing::{BillingInterval, PlanId};
use crate::config::SecretString;

pub const ENV_STRIPE_MODE: &str = "EYE_STRIPE_MODE";
pub const ENV_STRIPE_SECRET_KEY: &str = "EYE_STRIPE_SECRET_KEY";
pub const ENV_STRIPE_API_BASE: &str = "EYE_STRIPE_API_BASE";
pub const ENV_STRIPE_PRICE_GOLD_MONTHLY: &str = "EYE_STRIPE_PRICE_GOLD_MONTHLY";
pub const ENV_STRIPE_PRICE_GOLD_ANNUAL: &str = "EYE_STRIPE_PRICE_GOLD_ANNUAL";
pub const ENV_STRIPE_PRICE_PREMIUM_MONTHLY: &str = "EYE_STRIPE_PRICE_PREMIUM_MONTHLY";
pub const ENV_STRIPE_PRICE_PREMIUM_ANNUAL: &str = "EYE_STRIPE_PRICE_PREMIUM_ANNUAL";

pub const DEFAULT_STRIPE_API_BASE: &str = "https://api.stripe.com";
const MAX_PRICE_ID_LEN: usize = 255;

/// Which Stripe environment the settings target. Each mode requires its own key prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StripeMode {
    Test,
    Live,
}

/// One paid base-price slot in the catalog. Free plans have no Stripe price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PriceSlot {
    pub plan: PlanId,
    pub interval: BillingInterval,
}

impl PriceSlot {
    /// Every paid slot the catalog requires, in a fixed order.
    pub const REQUIRED: [PriceSlot; 4] = [
        PriceSlot {
            plan: PlanId::Gold,
            interval: BillingInterval::Monthly,
        },
        PriceSlot {
            plan: PlanId::Gold,
            interval: BillingInterval::Annual,
        },
        PriceSlot {
            plan: PlanId::Premium,
            interval: BillingInterval::Monthly,
        },
        PriceSlot {
            plan: PlanId::Premium,
            interval: BillingInterval::Annual,
        },
    ];

    pub fn label(self) -> &'static str {
        match (self.plan, self.interval) {
            (PlanId::Gold, BillingInterval::Monthly) => "gold_monthly",
            (PlanId::Gold, BillingInterval::Annual) => "gold_annual",
            (PlanId::Premium, BillingInterval::Monthly) => "premium_monthly",
            (PlanId::Premium, BillingInterval::Annual) => "premium_annual",
            (PlanId::Free, _) => "free",
        }
    }

    pub fn env_name(self) -> &'static str {
        match (self.plan, self.interval) {
            (PlanId::Gold, BillingInterval::Monthly) => ENV_STRIPE_PRICE_GOLD_MONTHLY,
            (PlanId::Gold, BillingInterval::Annual) => ENV_STRIPE_PRICE_GOLD_ANNUAL,
            (PlanId::Premium, BillingInterval::Monthly) => ENV_STRIPE_PRICE_PREMIUM_MONTHLY,
            (PlanId::Premium, BillingInterval::Annual) => ENV_STRIPE_PRICE_PREMIUM_ANNUAL,
            (PlanId::Free, _) => "",
        }
    }
}

/// Stripe price identifiers keyed by catalog slot. Every required slot is present, and no two
/// slots share an identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripePriceMap(BTreeMap<PriceSlot, String>);

impl StripePriceMap {
    pub fn get(&self, slot: PriceSlot) -> Option<&str> {
        self.0.get(&slot).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PriceSlot, &String)> {
        self.0.iter()
    }
}

/// Validated Stripe settings. Constructed only through [`StripeSettings::from_lookup`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeSettings {
    pub mode: StripeMode,
    pub secret_key: SecretString,
    pub api_base: String,
    pub prices: StripePriceMap,
}

/// Configuration failures. Names a variable or slot and a fixed reason, never a value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StripeConfigError {
    #[error("environment variable {name} is missing or invalid: {reason}")]
    Invalid {
        name: &'static str,
        reason: &'static str,
    },
    #[error("two catalog slots reference the same Stripe price identifier")]
    DuplicatePriceId,
    #[error("live Stripe mode is refused because the billing catalog is not approved")]
    LiveNotApproved,
}

impl StripeConfigError {
    pub fn code(&self) -> &'static str {
        match self {
            StripeConfigError::Invalid { .. } => "STRIPE_CONFIG_INVALID",
            StripeConfigError::DuplicatePriceId => "STRIPE_PRICE_ID_DUPLICATE",
            StripeConfigError::LiveNotApproved => "STRIPE_LIVE_NOT_APPROVED",
        }
    }
}

impl StripeSettings {
    /// Loads and validates Stripe settings from a lookup function.
    ///
    /// `catalog_approved` must be `true` only when the billing catalog's approval status is
    /// approved. Today it is always `false`, so live mode cannot load.
    pub fn from_lookup<F>(lookup: F, catalog_approved: bool) -> Result<Self, StripeConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let raw = |name: &str| {
            lookup(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };

        let mode = match raw(ENV_STRIPE_MODE).as_deref() {
            Some("test") => StripeMode::Test,
            Some("live") => StripeMode::Live,
            _ => {
                return Err(StripeConfigError::Invalid {
                    name: ENV_STRIPE_MODE,
                    reason: "must be \"test\" or \"live\"",
                });
            }
        };

        if mode == StripeMode::Live && !catalog_approved {
            return Err(StripeConfigError::LiveNotApproved);
        }

        let key = raw(ENV_STRIPE_SECRET_KEY).ok_or(StripeConfigError::Invalid {
            name: ENV_STRIPE_SECRET_KEY,
            reason: "is required",
        })?;
        let expected_prefix = match mode {
            StripeMode::Test => "sk_test_",
            StripeMode::Live => "sk_live_",
        };
        if !key.starts_with(expected_prefix)
            || key.len() <= expected_prefix.len()
            || key.bytes().any(|b| b.is_ascii_whitespace())
        {
            return Err(StripeConfigError::Invalid {
                name: ENV_STRIPE_SECRET_KEY,
                reason: "must be a secret key matching the configured mode",
            });
        }

        let api_base =
            raw(ENV_STRIPE_API_BASE).unwrap_or_else(|| DEFAULT_STRIPE_API_BASE.to_owned());
        validate_api_base(&api_base, mode)?;

        let mut map = BTreeMap::new();
        for slot in PriceSlot::REQUIRED {
            let value = raw(slot.env_name()).ok_or(StripeConfigError::Invalid {
                name: slot.env_name(),
                reason: "is required for every paid plan and interval",
            })?;
            if !valid_price_id(&value) {
                return Err(StripeConfigError::Invalid {
                    name: slot.env_name(),
                    reason: "must be a Stripe price identifier beginning with price_",
                });
            }
            map.insert(slot, value);
        }
        let mut seen = std::collections::HashSet::new();
        if !map.values().all(|value| seen.insert(value.clone())) {
            return Err(StripeConfigError::DuplicatePriceId);
        }

        Ok(StripeSettings {
            mode,
            secret_key: SecretString::new(key),
            api_base,
            prices: StripePriceMap(map),
        })
    }
}

/// Live mode may only use Stripe's own API host, so the live secret never leaves Stripe.
/// Test mode may use another https host or loopback http for the local mock harness.
fn validate_api_base(base: &str, mode: StripeMode) -> Result<(), StripeConfigError> {
    let invalid = StripeConfigError::Invalid {
        name: ENV_STRIPE_API_BASE,
        reason: "must be https://api.stripe.com, or a test-mode https or loopback http URL",
    };
    if base.bytes().any(|b| b.is_ascii_whitespace()) {
        return Err(invalid);
    }
    match mode {
        StripeMode::Live if base == DEFAULT_STRIPE_API_BASE => Ok(()),
        StripeMode::Live => Err(invalid),
        StripeMode::Test if base == DEFAULT_STRIPE_API_BASE => Ok(()),
        StripeMode::Test => {
            let https = base.starts_with("https://") && base.len() > "https://".len();
            let loopback =
                base.starts_with("http://127.0.0.1:") || base.starts_with("http://localhost:");
            if https || loopback {
                Ok(())
            } else {
                Err(invalid)
            }
        }
    }
}

fn valid_price_id(value: &str) -> bool {
    value.len() > "price_".len()
        && value.len() <= MAX_PRICE_ID_LEN
        && value.starts_with("price_")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl std::fmt::Display for StripeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StripeMode::Test => "test",
            StripeMode::Live => "live",
        })
    }
}
