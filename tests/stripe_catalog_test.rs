//! Client and catalog verification against a local mock Stripe server (issue #139).
//! No network access: the mock listens on 127.0.0.1 and the settings allow that loopback base in
//! test mode only.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};

use king_sparkon_eye::billing::stripe::config::{
    ENV_STRIPE_API_BASE, ENV_STRIPE_MODE, ENV_STRIPE_PRICE_GOLD_ANNUAL,
    ENV_STRIPE_PRICE_GOLD_MONTHLY, ENV_STRIPE_PRICE_PREMIUM_ANNUAL,
    ENV_STRIPE_PRICE_PREMIUM_MONTHLY, ENV_STRIPE_SECRET_KEY,
};
use king_sparkon_eye::billing::stripe::{
    PriceSlot, STRIPE_API_VERSION, StripeClient, StripeError, StripeSettings, verify_catalog,
    verify_price_slot,
};
use king_sparkon_eye::billing::{BillingInterval, PlanId};

const TEST_KEY: &str = "sk_test_placeholder_not_a_real_key";
const GOLD_MONTHLY: &str = "price_gold_monthly";
const GOLD_ANNUAL: &str = "price_gold_annual";
const PREMIUM_MONTHLY: &str = "price_premium_monthly";
const PREMIUM_ANNUAL: &str = "price_premium_annual";

fn slot(plan: PlanId, interval: BillingInterval) -> PriceSlot {
    PriceSlot { plan, interval }
}

// ---------- mock server ----------

type Responder = Arc<dyn Fn(&str) -> Response + Send + Sync>;

#[derive(Debug, PartialEq)]
struct SeenRequest {
    id: String,
    authorization: String,
    stripe_version: String,
}

#[derive(Clone, Default)]
struct Seen {
    requests: Arc<Mutex<Vec<SeenRequest>>>,
    followed: Arc<Mutex<u32>>,
}

#[derive(Clone)]
struct MockState {
    respond: Responder,
    seen: Seen,
}

async fn handle_price(
    State(state): State<MockState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let read = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    };
    state.seen.requests.lock().unwrap().push(SeenRequest {
        id: id.clone(),
        authorization: read("authorization"),
        stripe_version: read("stripe-version"),
    });
    (state.respond)(&id)
}

async fn followed(State(state): State<MockState>) -> Response {
    *state.seen.followed.lock().unwrap() += 1;
    StatusCode::OK.into_response()
}

async fn spawn(respond: impl Fn(&str) -> Response + Send + Sync + 'static) -> (String, Seen) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let seen = Seen::default();
    let respond: Responder = Arc::new(respond);
    let state = MockState {
        respond,
        seen: seen.clone(),
    };
    let app = Router::new()
        .route("/v1/prices/{id}", get(handle_price))
        .route("/v1/followed", get(followed))
        .with_state(state);
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    (format!("http://{addr}"), seen)
}

fn json_reply(status: u16, body: &Value) -> Response {
    (
        StatusCode::from_u16(status).expect("status"),
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

fn status_reply(status: u16) -> Response {
    StatusCode::from_u16(status)
        .expect("status")
        .into_response()
}

fn price(
    id: &str,
    active: bool,
    currency: &str,
    livemode: bool,
    recurring: Option<(&str, &str)>,
) -> Value {
    json!({
        "id": id,
        "object": "price",
        "active": active,
        "currency": currency,
        "livemode": livemode,
        "recurring": recurring.map(|(interval, usage)| json!({ "interval": interval, "usage_type": usage })),
    })
}

/// A price that passes every check for the slot implied by its identifier.
fn healthy_price(id: &str) -> Value {
    let interval = if id.ends_with("_annual") {
        "year"
    } else {
        "month"
    };
    price(id, true, "usd", false, Some((interval, "licensed")))
}

fn healthy_responder(id: &str) -> Response {
    json_reply(200, &healthy_price(id))
}

// ---------- settings and client helpers ----------

fn settings_for(api_base: &str) -> StripeSettings {
    let env: HashMap<&'static str, String> = HashMap::from([
        (ENV_STRIPE_MODE, "test".to_owned()),
        (ENV_STRIPE_SECRET_KEY, TEST_KEY.to_owned()),
        (ENV_STRIPE_API_BASE, api_base.to_owned()),
        (ENV_STRIPE_PRICE_GOLD_MONTHLY, GOLD_MONTHLY.to_owned()),
        (ENV_STRIPE_PRICE_GOLD_ANNUAL, GOLD_ANNUAL.to_owned()),
        (ENV_STRIPE_PRICE_PREMIUM_MONTHLY, PREMIUM_MONTHLY.to_owned()),
        (ENV_STRIPE_PRICE_PREMIUM_ANNUAL, PREMIUM_ANNUAL.to_owned()),
    ]);
    StripeSettings::from_lookup(move |name| env.get(name).cloned(), false)
        .expect("valid test settings")
}

fn client_for(settings: &StripeSettings) -> StripeClient {
    StripeClient::new(settings).expect("client")
}

fn mismatch(slot_label: &'static str, reason: &'static str) -> StripeError {
    StripeError::CatalogMismatch {
        slot: slot_label,
        reason,
    }
}

// ---------- request shape ----------

#[tokio::test]
async fn request_carries_bearer_auth_and_the_pinned_api_version() {
    let (base, seen) = spawn(healthy_responder).await;
    let settings = settings_for(&base);
    let client = client_for(&settings);

    verify_price_slot(
        &client,
        slot(PlanId::Gold, BillingInterval::Monthly),
        GOLD_MONTHLY,
    )
    .await
    .expect("healthy price verifies");

    let requests = seen.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].id, GOLD_MONTHLY);
    assert_eq!(requests[0].authorization, format!("Bearer {TEST_KEY}"));
    assert_eq!(requests[0].stripe_version, STRIPE_API_VERSION);
}

#[tokio::test]
async fn full_catalog_of_healthy_prices_verifies_every_slot() {
    let (base, seen) = spawn(healthy_responder).await;
    let settings = settings_for(&base);
    let client = client_for(&settings);

    verify_catalog(&client, &settings)
        .await
        .expect("catalog verifies");

    let requests = seen.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    for expected in [GOLD_MONTHLY, GOLD_ANNUAL, PREMIUM_MONTHLY, PREMIUM_ANNUAL] {
        assert!(
            requests.iter().any(|r| r.id == expected),
            "missing request for {expected}"
        );
    }
}

#[tokio::test]
async fn missing_usage_type_is_treated_as_licensed() {
    let (base, _) = spawn(|id| {
        json_reply(
            200,
            &json!({
                "id": id, "object": "price", "active": true, "currency": "usd",
                "livemode": false, "recurring": { "interval": "month" }
            }),
        )
    })
    .await;
    let settings = settings_for(&base);
    let client = client_for(&settings);

    verify_price_slot(
        &client,
        slot(PlanId::Gold, BillingInterval::Monthly),
        GOLD_MONTHLY,
    )
    .await
    .expect("absent usage_type defaults to licensed");
}

// ---------- catalog-slot mismatches ----------

async fn expect_mismatch(body: Value, slot_label: &'static str, reason: &'static str) {
    let (base, _) = spawn(move |_| json_reply(200, &body)).await;
    let settings = settings_for(&base);
    let client = client_for(&settings);
    let result = verify_price_slot(
        &client,
        slot(PlanId::Gold, BillingInterval::Monthly),
        GOLD_MONTHLY,
    )
    .await;
    assert_eq!(result, Err(mismatch(slot_label, reason)));
}

#[tokio::test]
async fn returned_identifier_must_match_the_request() {
    expect_mismatch(
        healthy_price("price_someone_else"),
        "gold_monthly",
        "returned identifier does not match the request",
    )
    .await;
}

#[tokio::test]
async fn live_price_is_rejected_by_a_test_configuration() {
    expect_mismatch(
        price(GOLD_MONTHLY, true, "usd", true, Some(("month", "licensed"))),
        "gold_monthly",
        "livemode does not match the configured mode",
    )
    .await;
}

#[tokio::test]
async fn inactive_price_is_rejected() {
    expect_mismatch(
        price(
            GOLD_MONTHLY,
            false,
            "usd",
            false,
            Some(("month", "licensed")),
        ),
        "gold_monthly",
        "price is inactive",
    )
    .await;
}

#[tokio::test]
async fn non_usd_price_is_rejected() {
    expect_mismatch(
        price(
            GOLD_MONTHLY,
            true,
            "eur",
            false,
            Some(("month", "licensed")),
        ),
        "gold_monthly",
        "currency is not USD",
    )
    .await;
}

#[tokio::test]
async fn one_time_price_is_rejected() {
    expect_mismatch(
        price(GOLD_MONTHLY, true, "usd", false, None),
        "gold_monthly",
        "price is not recurring",
    )
    .await;
}

#[tokio::test]
async fn wrong_interval_is_rejected() {
    expect_mismatch(
        price(GOLD_MONTHLY, true, "usd", false, Some(("year", "licensed"))),
        "gold_monthly",
        "interval does not match the slot",
    )
    .await;
}

#[tokio::test]
async fn metered_base_price_is_rejected() {
    expect_mismatch(
        price(GOLD_MONTHLY, true, "usd", false, Some(("month", "metered"))),
        "gold_monthly",
        "base price must be licensed, not metered",
    )
    .await;
}

#[tokio::test]
async fn catalog_stops_at_the_first_bad_slot_and_names_it() {
    let (base, _) = spawn(|id| {
        if id == PREMIUM_ANNUAL {
            json_reply(
                200,
                &price(id, false, "usd", false, Some(("year", "licensed"))),
            )
        } else {
            healthy_responder(id)
        }
    })
    .await;
    let settings = settings_for(&base);
    let client = client_for(&settings);

    let err = verify_catalog(&client, &settings)
        .await
        .expect_err("one slot is inactive");
    assert_eq!(err, mismatch("premium_annual", "price is inactive"));
    assert_eq!(err.code(), "STRIPE_CATALOG_MISMATCH");
    assert!(
        !err.to_string().contains("price_"),
        "error must not echo identifiers"
    );
}

// ---------- remote failures ----------

#[tokio::test]
async fn http_error_statuses_map_to_safe_categories() {
    let cases = [
        (401, StripeError::Rejected { status: 401 }),
        (404, StripeError::Rejected { status: 404 }),
        (429, StripeError::Unavailable),
        (500, StripeError::Unavailable),
        (503, StripeError::Unavailable),
    ];
    for (status, expected) in cases {
        let (base, _) = spawn(move |_| status_reply(status)).await;
        let settings = settings_for(&base);
        let client = client_for(&settings);
        let result = verify_price_slot(
            &client,
            slot(PlanId::Gold, BillingInterval::Monthly),
            GOLD_MONTHLY,
        )
        .await;
        assert_eq!(result, Err(expected), "status {status}");
    }
}

#[tokio::test]
async fn malformed_success_body_is_an_unexpected_response() {
    let (base, _) = spawn(|_| (StatusCode::OK, "this is not json").into_response()).await;
    let settings = settings_for(&base);
    let client = client_for(&settings);
    let result = verify_price_slot(
        &client,
        slot(PlanId::Gold, BillingInterval::Monthly),
        GOLD_MONTHLY,
    )
    .await;
    assert_eq!(result, Err(StripeError::UnexpectedResponse));
}

#[tokio::test]
async fn redirects_are_not_followed_so_the_key_is_not_forwarded() {
    let (base, seen) =
        spawn(|_| (StatusCode::FOUND, [(header::LOCATION, "/v1/followed")]).into_response()).await;
    let settings = settings_for(&base);
    let client = client_for(&settings);

    let result = verify_price_slot(
        &client,
        slot(PlanId::Gold, BillingInterval::Monthly),
        GOLD_MONTHLY,
    )
    .await;
    assert_eq!(result, Err(StripeError::Rejected { status: 302 }));
    assert_eq!(
        *seen.followed.lock().unwrap(),
        0,
        "redirect target must not be contacted"
    );
}

#[tokio::test]
async fn unreachable_host_is_reported_as_unavailable() {
    // Bind then drop, so the port is closed when the client connects.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        listener.local_addr().expect("addr").port()
    };
    let settings = settings_for(&format!("http://127.0.0.1:{port}"));
    let client = client_for(&settings);

    let result = verify_price_slot(
        &client,
        slot(PlanId::Gold, BillingInterval::Monthly),
        GOLD_MONTHLY,
    )
    .await;
    assert_eq!(result, Err(StripeError::Unavailable));
}

#[tokio::test]
async fn error_display_and_debug_never_contain_the_secret() {
    let (base, _) = spawn(|_| status_reply(401)).await;
    let settings = settings_for(&base);
    let client = client_for(&settings);
    let err = verify_catalog(&client, &settings)
        .await
        .expect_err("401 rejects");
    assert!(!err.to_string().contains("placeholder_not_a_real_key"));
    assert!(!format!("{err:?}").contains("placeholder_not_a_real_key"));
}
