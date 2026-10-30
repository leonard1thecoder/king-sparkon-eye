use std::collections::HashMap;

use king_sparkon_eye::billing::stripe::config::{
    ENV_STRIPE_API_BASE, ENV_STRIPE_MODE, ENV_STRIPE_PRICE_GOLD_ANNUAL,
    ENV_STRIPE_PRICE_GOLD_MONTHLY, ENV_STRIPE_PRICE_PREMIUM_ANNUAL,
    ENV_STRIPE_PRICE_PREMIUM_MONTHLY, ENV_STRIPE_SECRET_KEY,
};
use king_sparkon_eye::billing::stripe::{PriceSlot, StripeConfigError, StripeMode, StripeSettings};
use king_sparkon_eye::billing::{BillingInterval, PlanId};

const TEST_KEY: &str = "sk_test_placeholder_not_a_real_key";
const LIVE_KEY: &str = "sk_live_placeholder_not_a_real_key";

fn base_env(mode: &'static str, key: &'static str) -> HashMap<&'static str, String> {
    HashMap::from([
        (ENV_STRIPE_MODE, mode.to_owned()),
        (ENV_STRIPE_SECRET_KEY, key.to_owned()),
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
    ])
}

fn load(
    env: &HashMap<&'static str, String>,
    approved: bool,
) -> Result<StripeSettings, StripeConfigError> {
    let env = env.clone();
    StripeSettings::from_lookup(move |name| env.get(name).cloned(), approved)
}

#[test]
fn valid_test_settings_load_with_all_four_paid_slots() {
    let settings = load(&base_env("test", TEST_KEY), false).expect("valid test config");
    assert_eq!(settings.mode, StripeMode::Test);
    assert_eq!(settings.api_base, "https://api.stripe.com");
    for slot in PriceSlot::REQUIRED {
        assert!(settings.prices.get(slot).is_some(), "missing {slot:?}");
    }
    assert_eq!(
        settings.prices.get(PriceSlot {
            plan: PlanId::Gold,
            interval: BillingInterval::Annual
        }),
        Some("price_gold_annual")
    );
}

#[test]
fn live_mode_is_refused_while_the_catalog_is_unapproved() {
    let err = load(&base_env("live", LIVE_KEY), false).expect_err("live must be refused");
    assert_eq!(err, StripeConfigError::LiveNotApproved);
    assert_eq!(err.code(), "STRIPE_LIVE_NOT_APPROVED");
}

#[test]
fn live_mode_loads_only_with_an_approved_catalog_and_live_key() {
    let settings = load(&base_env("live", LIVE_KEY), true).expect("approved live config");
    assert_eq!(settings.mode, StripeMode::Live);
}

#[test]
fn live_mode_rejects_a_test_key() {
    let err = load(&base_env("live", TEST_KEY), true).expect_err("key/mode mismatch");
    assert!(
        matches!(err, StripeConfigError::Invalid { name, .. } if name == ENV_STRIPE_SECRET_KEY)
    );
}

#[test]
fn test_mode_rejects_a_live_key() {
    let err = load(&base_env("test", LIVE_KEY), false).expect_err("key/mode mismatch");
    assert!(
        matches!(err, StripeConfigError::Invalid { name, .. } if name == ENV_STRIPE_SECRET_KEY)
    );
}

#[test]
fn mode_must_be_explicit() {
    let mut env = base_env("test", TEST_KEY);
    env.remove(ENV_STRIPE_MODE);
    assert!(
        matches!(load(&env, false), Err(StripeConfigError::Invalid { name, .. }) if name == ENV_STRIPE_MODE)
    );
    env.insert(ENV_STRIPE_MODE, "sandbox".to_owned());
    assert!(
        matches!(load(&env, false), Err(StripeConfigError::Invalid { name, .. }) if name == ENV_STRIPE_MODE)
    );
}

#[test]
fn key_with_whitespace_or_empty_body_is_rejected() {
    for bad in ["sk_test_", "sk_test_has space", "rk_test_restricted"] {
        let mut env = base_env("test", TEST_KEY);
        env.insert(ENV_STRIPE_SECRET_KEY, bad.to_owned());
        assert!(load(&env, false).is_err(), "expected {bad:?} rejected");
    }
}

#[test]
fn every_paid_slot_is_required() {
    for name in [
        ENV_STRIPE_PRICE_GOLD_MONTHLY,
        ENV_STRIPE_PRICE_GOLD_ANNUAL,
        ENV_STRIPE_PRICE_PREMIUM_MONTHLY,
        ENV_STRIPE_PRICE_PREMIUM_ANNUAL,
    ] {
        let mut env = base_env("test", TEST_KEY);
        env.remove(name);
        assert!(
            matches!(load(&env, false), Err(StripeConfigError::Invalid { name: n, .. }) if n == name),
            "{name}"
        );
    }
}

#[test]
fn price_identifiers_must_look_like_stripe_prices() {
    for bad in ["prod_123", "price_", "price_has-dash", "price_has space"] {
        let mut env = base_env("test", TEST_KEY);
        env.insert(ENV_STRIPE_PRICE_GOLD_MONTHLY, bad.to_owned());
        assert!(load(&env, false).is_err(), "expected {bad:?} rejected");
    }
}

#[test]
fn one_price_cannot_back_two_slots() {
    let mut env = base_env("test", TEST_KEY);
    env.insert(
        ENV_STRIPE_PRICE_GOLD_ANNUAL,
        "price_gold_monthly".to_owned(),
    );
    assert_eq!(load(&env, false), Err(StripeConfigError::DuplicatePriceId));
}

#[test]
fn api_base_is_https_for_test_and_loopback_only_in_test() {
    let mut env = base_env("test", TEST_KEY);
    env.insert(ENV_STRIPE_API_BASE, "http://127.0.0.1:9999".to_owned());
    assert!(load(&env, false).is_ok());

    env.insert(ENV_STRIPE_API_BASE, "http://example.com".to_owned());
    assert!(load(&env, false).is_err());

    env.insert(ENV_STRIPE_API_BASE, "https://proxy.example.test".to_owned());
    assert!(load(&env, false).is_ok());
}

#[test]
fn live_mode_only_sends_the_key_to_stripe_itself() {
    let mut env = base_env("live", LIVE_KEY);
    env.insert(ENV_STRIPE_API_BASE, "https://proxy.example.test".to_owned());
    assert!(load(&env, true).is_err());

    env.insert(ENV_STRIPE_API_BASE, "http://127.0.0.1:9999".to_owned());
    assert!(load(&env, true).is_err());

    env.insert(ENV_STRIPE_API_BASE, "https://api.stripe.com".to_owned());
    assert!(load(&env, true).is_ok());
}

#[test]
fn secrets_never_appear_in_debug_or_error_output() {
    let settings = load(&base_env("test", TEST_KEY), false).expect("valid");
    assert!(!format!("{settings:?}").contains("placeholder_not_a_real_key"));

    let mut env = base_env("test", TEST_KEY);
    env.insert(ENV_STRIPE_PRICE_GOLD_MONTHLY, "bad value".to_owned());
    let err = load(&env, false).expect_err("invalid price");
    assert!(!format!("{err}").contains("bad value"));
    assert!(!format!("{err:?}").contains("placeholder_not_a_real_key"));
}

#[test]
fn slot_labels_and_env_names_are_stable() {
    let slot = PriceSlot {
        plan: PlanId::Premium,
        interval: BillingInterval::Annual,
    };
    assert_eq!(slot.label(), "premium_annual");
    assert_eq!(slot.env_name(), ENV_STRIPE_PRICE_PREMIUM_ANNUAL);
}
