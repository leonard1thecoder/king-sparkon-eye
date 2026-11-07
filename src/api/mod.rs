//! HTTP routes: health probes, bounded photo ingress, OAuth, and request-ID/tracing middleware.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use axum::body::{Body, to_bytes};
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode, header::AUTHORIZATION};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::auth::jwt::AccessTokenClaims;
use crate::auth::oauth_runtime::OAuthRuntime;
use crate::auth::{BoundaryError, CallerIdentity, RouteGuard, verify_access_token};
use crate::billing::runtime::BillingRuntime;
use crate::billing::stripe::{PriceSlot, StripeError};
use crate::billing::{BillingInterval, PlanId};
use crate::config::Config;
use crate::contract::{PersistenceStatus, PhotoAssessmentResponse};
use crate::domain::{AssessmentConfig, assess, conservative_result};
use crate::error::{ApiError, ValidationError};
use crate::inference::yunet::YunetDetector;
use crate::ingress::ingest_with_timeout;
use crate::persistence::billing_accounts::{
    BillingAccountProvisionError, ensure_customer_mapping, validate_eye_user_id,
};
use crate::provider::DisabledProvider;

pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub oauth: Option<Arc<OAuthRuntime>>,
    billing: Option<Arc<BillingRuntime>>,
    assessment: Option<Arc<AssessmentRuntime>>,
}

struct AssessmentRuntime {
    detector: Arc<YunetDetector>,
}

static YUNET_DETECTOR: OnceLock<Result<Arc<YunetDetector>, crate::inference::InferenceError>> =
    OnceLock::new();

fn load_detector() -> Result<Arc<YunetDetector>, crate::inference::InferenceError> {
    YUNET_DETECTOR
        .get_or_init(|| {
            let root = Path::new(env!("CARGO_MANIFEST_DIR"));
            let model_path = std::env::var_os("EYE_YUNET_MODEL_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    root.join("artifacts/models/yunet-2023mar/face_detection_yunet_2023mar.onnx")
                });
            let runtime_path = std::env::var_os("ORT_DYLIB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    root.join("artifacts/onnxruntime/1.31.1/linux-x86_64/libonnxruntime.so.1.31.1")
                });
            YunetDetector::load(&model_path, &runtime_path).map(Arc::new)
        })
        .clone()
}

fn assessment_runtime() -> Option<Arc<AssessmentRuntime>> {
    match load_detector() {
        Ok(detector) => Some(Arc::new(AssessmentRuntime { detector })),
        Err(error) => {
            tracing::error!(?error, "YuNet assessment runtime could not be loaded");
            None
        }
    }
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct LivenessResponse {
    pub status: &'static str,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DependencyState {
    /// Dependency is enabled and fully configured.
    Ok,
    /// Dependency is intentionally disabled; not a readiness failure.
    Disabled,
    /// Dependency is enabled but its credentials are missing; readiness fails.
    MissingCredentials,
    /// Required runtime artifact or dependency could not be loaded.
    Unavailable,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct DependencyStatus {
    pub name: &'static str,
    pub required: bool,
    pub state: DependencyState,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessStatus {
    Ready,
    NotReady,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ReadinessResponse {
    pub status: ReadinessStatus,
    pub dependencies: Vec<DependencyStatus>,
}

/// Computes dependency states from configuration only. No network calls are made here.
pub fn dependency_statuses(config: &Config) -> Vec<DependencyStatus> {
    let vision_state = match (
        config.vision_provider_enabled,
        config.vision_provider_api_key.is_some(),
    ) {
        (false, _) => DependencyState::Disabled,
        (true, true) => DependencyState::Ok,
        (true, false) => DependencyState::MissingCredentials,
    };
    vec![DependencyStatus {
        name: "vision_provider",
        required: config.vision_provider_enabled,
        state: vision_state,
    }]
}

pub fn readiness(config: &Config) -> ReadinessResponse {
    let dependencies = dependency_statuses(config);
    let ready = dependencies
        .iter()
        .all(|d| !d.required || d.state == DependencyState::Ok);
    ReadinessResponse {
        status: if ready {
            ReadinessStatus::Ready
        } else {
            ReadinessStatus::NotReady
        },
        dependencies,
    }
}

async fn live() -> Json<LivenessResponse> {
    Json(LivenessResponse { status: "ok" })
}

/// Marks operational, account and API responses as non-indexable even if discovered externally.
async fn add_noindex_header(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        HeaderName::from_static("x-robots-tag"),
        HeaderValue::from_static("noindex, nofollow, noarchive"),
    );
    response
}

async fn ready(State(state): State<AppState>) -> Response {
    let mut report = readiness(&state.config);
    if state.assessment.is_none() {
        report.dependencies.push(DependencyStatus {
            name: "face_detector",
            required: true,
            state: DependencyState::Unavailable,
        });
        report.status = ReadinessStatus::NotReady;
    }
    let code = match report.status {
        ReadinessStatus::Ready => StatusCode::OK,
        ReadinessStatus::NotReady => StatusCode::SERVICE_UNAVAILABLE,
    };
    (code, Json(report)).into_response()
}

const MULTIPART_OVERHEAD_BYTES: usize = 64 * 1024;
/// Minimum HS256 signing-secret length, matching the configuration check.
const MIN_JWT_SECRET_BYTES: usize = 32;

fn current_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Authenticates `POST /v1/enrollment/photo-assessments`: a bearer JWT with `enrollment:write`.
///
/// Fails closed. A missing or short signing secret, a missing or malformed header, and any
/// token that does not verify all return the same `401`. A verified token without the scope
/// returns `403`. Runs before the body is read, so unauthenticated callers cannot reach ingress.
fn authenticate_enrollment(
    config: &crate::config::Config,
    headers: &HeaderMap,
) -> Result<CallerIdentity, ApiError> {
    authenticate_enrollment_at(config, headers, current_unix_seconds())
}

fn authenticate_enrollment_at(
    config: &crate::config::Config,
    headers: &HeaderMap,
    now_secs: u64,
) -> Result<CallerIdentity, ApiError> {
    // The signing secret and key ID belong to the OAuth runtime configuration. When OAuth is
    // not fully configured there is no verification key, so every request fails closed.
    let oauth = config.oauth.as_ref().ok_or(ApiError::Unauthorized)?;
    let secret = oauth.jwt_signing_secret.expose();
    if secret.len() < MIN_JWT_SECRET_BYTES {
        return Err(ApiError::Unauthorized);
    }
    let header = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    let token = header
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim())
        .filter(|token| !token.is_empty())
        .ok_or(ApiError::Unauthorized)?;
    let claims = verify_access_token(token, &oauth.jwt_kid, secret.as_bytes(), now_secs)
        .map_err(|_| ApiError::Unauthorized)?;
    let caller = CallerIdentity::User(claims);
    RouteGuard::authorize_enrollment(&caller).map_err(|error| match error {
        BoundaryError::ForbiddenScope | BoundaryError::CrossEventAccessDenied => {
            ApiError::Forbidden
        }
        BoundaryError::Unauthorized => ApiError::Unauthorized,
    })?;
    Ok(caller)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CheckoutRequest {
    plan: String,
    interval: String,
    success_url: String,
    cancel_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PortalRequest {
    return_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BillingSessionResponse {
    url: String,
}

fn authenticate_billing(
    config: &Config,
    headers: &HeaderMap,
) -> Result<AccessTokenClaims, ApiError> {
    let oauth = config.oauth.as_ref().ok_or(ApiError::Unauthorized)?;
    let secret = oauth.jwt_signing_secret.expose();
    if secret.len() < MIN_JWT_SECRET_BYTES {
        return Err(ApiError::Unauthorized);
    }
    let header = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::Unauthorized)?;
    let token = header
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim())
        .filter(|token| !token.is_empty())
        .ok_or(ApiError::Unauthorized)?;
    let claims = verify_access_token(
        token,
        &oauth.jwt_kid,
        secret.as_bytes(),
        current_unix_seconds(),
    )
    .map_err(|_| ApiError::Unauthorized)?;
    validate_eye_user_id(&claims.sub).map_err(|_| ApiError::Unauthorized)?;
    Ok(claims)
}

fn billing_idempotency_key(
    owner_id: &str,
    headers: &HeaderMap,
    operation: &'static str,
) -> Result<String, ApiError> {
    let supplied = headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::Validation(ValidationError {
            field: "Idempotency-Key",
            rule: "header is required",
        }))?;
    if supplied.is_empty()
        || supplied.len() > 255
        || !supplied
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
    {
        return Err(ApiError::Validation(ValidationError {
            field: "Idempotency-Key",
            rule: "must contain 1 to 255 ASCII letters, numbers, underscores or hyphens",
        }));
    }
    let digest = Sha256::digest(format!("eye:{owner_id}:{operation}:{supplied}").as_bytes());
    let suffix = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(format!("eye-{operation}-{suffix}"))
}

fn checkout_slot(plan: &str, interval: &str) -> Result<PriceSlot, ApiError> {
    let plan = match plan {
        "gold" => PlanId::Gold,
        "premium" => PlanId::Premium,
        _ => {
            return Err(ApiError::Validation(ValidationError {
                field: "plan",
                rule: "must be one of the configured paid plans",
            }));
        }
    };
    let interval = match interval {
        "monthly" => BillingInterval::Monthly,
        "annual" => BillingInterval::Annual,
        _ => {
            return Err(ApiError::Validation(ValidationError {
                field: "interval",
                rule: "must be monthly or annual",
            }));
        }
    };
    Ok(PriceSlot { plan, interval })
}

fn map_billing_provision_error(error: BillingAccountProvisionError) -> ApiError {
    match error {
        BillingAccountProvisionError::InvalidOwner => ApiError::Unauthorized,
        BillingAccountProvisionError::StripeUnavailable
        | BillingAccountProvisionError::MappingConflict
        | BillingAccountProvisionError::Unavailable => ApiError::ServiceUnavailable,
    }
}

fn map_stripe_error(error: StripeError) -> ApiError {
    match error {
        StripeError::InvalidRequest { .. } => ApiError::Validation(ValidationError {
            field: "billing",
            rule: "request or configured billing parameters are invalid",
        }),
        StripeError::Unavailable
        | StripeError::Rejected { .. }
        | StripeError::UnexpectedResponse
        | StripeError::CatalogMismatch { .. } => ApiError::ServiceUnavailable,
    }
}

async fn create_checkout(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Json<BillingSessionResponse>, ApiError> {
    let owner_id = authenticate_billing(&state.config, request.headers())?.sub;
    let idempotency_key = billing_idempotency_key(&owner_id, request.headers(), "checkout")?;
    if !request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return Err(ApiError::UnsupportedMediaType);
    }
    let body = to_bytes(request.into_body(), 16 * 1024)
        .await
        .map_err(|_| ApiError::PayloadTooLarge)?;
    let request: CheckoutRequest = serde_json::from_slice(&body).map_err(|_| {
        ApiError::Validation(ValidationError {
            field: "body",
            rule: "must be a valid JSON billing request",
        })
    })?;
    let slot = checkout_slot(&request.plan, &request.interval)?;
    let runtime = state.billing.as_ref().ok_or(ApiError::ServiceUnavailable)?;
    runtime
        .return_urls
        .validate(&request.success_url)
        .map_err(|_| {
            ApiError::Validation(ValidationError {
                field: "successUrl",
                rule: "must use a configured HTTPS return origin",
            })
        })?;
    runtime
        .return_urls
        .validate(&request.cancel_url)
        .map_err(|_| {
            ApiError::Validation(ValidationError {
                field: "cancelUrl",
                rule: "must use a configured HTTPS return origin",
            })
        })?;
    let customer_id = ensure_customer_mapping(&runtime.store, &runtime.stripe, &owner_id)
        .await
        .map_err(map_billing_provision_error)?;
    let session = runtime
        .stripe
        .create_checkout_session(
            slot,
            &customer_id,
            &request.success_url,
            &request.cancel_url,
            &runtime.return_urls,
            &idempotency_key,
        )
        .await
        .map_err(map_stripe_error)?;
    Ok(Json(BillingSessionResponse { url: session.url }))
}

async fn create_customer_portal(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Json<BillingSessionResponse>, ApiError> {
    let owner_id = authenticate_billing(&state.config, request.headers())?.sub;
    let idempotency_key = billing_idempotency_key(&owner_id, request.headers(), "portal")?;
    if !request
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return Err(ApiError::UnsupportedMediaType);
    }
    let body = to_bytes(request.into_body(), 16 * 1024)
        .await
        .map_err(|_| ApiError::PayloadTooLarge)?;
    let request: PortalRequest = serde_json::from_slice(&body).map_err(|_| {
        ApiError::Validation(ValidationError {
            field: "body",
            rule: "must be a valid JSON billing request",
        })
    })?;
    let runtime = state.billing.as_ref().ok_or(ApiError::ServiceUnavailable)?;
    runtime
        .return_urls
        .validate(&request.return_url)
        .map_err(|_| {
            ApiError::Validation(ValidationError {
                field: "returnUrl",
                rule: "must use a configured HTTPS return origin",
            })
        })?;
    let customer_id = ensure_customer_mapping(&runtime.store, &runtime.stripe, &owner_id)
        .await
        .map_err(map_billing_provision_error)?;
    let session = runtime
        .stripe
        .create_portal_session(
            &customer_id,
            &request.return_url,
            &runtime.return_urls,
            &idempotency_key,
        )
        .await
        .map_err(map_stripe_error)?;
    Ok(Json(BillingSessionResponse { url: session.url }))
}

async fn upload_photo(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    authenticate_enrollment(&state.config, request.headers())?;
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    // Authentication runs before Multipart consumes any request-body bytes.
    let mut multipart = Multipart::from_request(request, &state)
        .await
        .map_err(|error| {
            if error.into_response().status() == StatusCode::PAYLOAD_TOO_LARGE {
                ApiError::PayloadTooLarge
            } else {
                ApiError::UnsupportedMediaType
            }
        })?;
    let mut image = None;
    while let Some(mut field) = multipart.next_field().await.map_err(|error| {
        if error.into_response().status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError::PayloadTooLarge
        } else {
            invalid_multipart()
        }
    })? {
        let field_name = field.name().unwrap_or_default().to_owned();
        match field_name.as_str() {
            "image" => {
                if image.is_some() {
                    return Err(invalid_multipart());
                }
                let mut bytes = Vec::new();
                while let Some(chunk) =
                    field.chunk().await.map_err(|_| ApiError::PayloadTooLarge)?
                {
                    if bytes.len().saturating_add(chunk.len())
                        > state.config.image_limits.max_upload_bytes
                    {
                        return Err(ApiError::PayloadTooLarge);
                    }
                    bytes.extend_from_slice(&chunk);
                }
                image = Some(bytes);
            }
            "clientRequestId" => {
                let mut size = 0usize;
                while let Some(chunk) =
                    field.chunk().await.map_err(|_| ApiError::PayloadTooLarge)?
                {
                    size = size.saturating_add(chunk.len());
                    if size > 1024 {
                        return Err(invalid_multipart());
                    }
                }
            }
            _ => return Err(invalid_multipart()),
        }
    }
    let image = image.ok_or_else(missing_image)?;
    let normalized = ingest_with_timeout(image, state.config.image_limits)
        .await
        .map_err(ApiError::from)?;
    let runtime = state
        .assessment
        .as_ref()
        .ok_or(ApiError::ServiceUnavailable)?;
    let disabled_provider = DisabledProvider;
    let result = assess(
        &normalized,
        runtime.detector.as_ref(),
        &disabled_provider,
        &AssessmentConfig::default(),
    )
    .await;
    let result = conservative_result(result);
    let model_version = None;
    let response = PhotoAssessmentResponse {
        assessment_id: uuid::Uuid::new_v4().to_string(),
        request_id: request_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        suitability: result.suitability,
        reason_codes: result.reason_codes,
        persistence: PersistenceStatus::NotRequested,
        model_version,
        policy_version: "photo-quality-uncalibrated-v1".to_owned(),
    };
    Ok((StatusCode::OK, Json(response)).into_response())
}

fn invalid_multipart() -> ApiError {
    ValidationError {
        field: "image",
        rule: "must be provided in a valid multipart/form-data request",
    }
    .into()
}

fn missing_image() -> ApiError {
    ValidationError {
        field: "image",
        rule: "is required",
    }
    .into()
}

async fn not_found() -> ApiError {
    ApiError::NotFound
}

/// Connect to Eye PostgreSQL and apply the embedded schema before the HTTP listener starts.
/// The pool is intentionally closed here; the request runtime opens its own bounded pool.
pub async fn initialize_auth_storage(config: &Config) -> Result<(), ()> {
    let Some(oauth) = config.oauth.as_ref() else {
        return Ok(());
    };
    let pool = crate::persistence::connect(&oauth.database_url)
        .await
        .map_err(|_| ())?;
    crate::persistence::migrate(&pool).await.map_err(|_| ())?;
    pool.close().await;
    Ok(())
}

pub fn router(config: Config) -> Router {
    router_with_billing(config, None)
}

pub fn router_with_billing(config: Config, billing: Option<Arc<BillingRuntime>>) -> Router {
    let oauth = config.oauth.clone().and_then(|oauth_config| {
        match OAuthRuntime::new(oauth_config) {
            Ok(runtime) => Some(Arc::new(runtime)),
            Err(_) => {
                // Never echo OAuth settings or secrets. The four auth routes remain mounted
                // but fail closed with 503 while health/readiness continues to expose service status.
                tracing::error!("OAuth runtime initialization failed; OAuth routes are disabled");
                None
            }
        }
    });
    let upload_body_limit = config
        .image_limits
        .max_upload_bytes
        .saturating_add(MULTIPART_OVERHEAD_BYTES);
    let assessment = assessment_runtime();
    let state = AppState {
        config: Arc::new(config),
        oauth,
        billing,
        assessment,
    };
    let non_indexable_routes = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route(
            "/v1/enrollment/photo-assessments",
            post(upload_photo).layer(DefaultBodyLimit::max(upload_body_limit)),
        )
        .route("/v1/billing/checkout", post(create_checkout))
        .route("/v1/billing/portal", post(create_customer_portal))
        .merge(OAuthRuntime::routes())
        .merge(crate::auth::pages::routes())
        .route_layer(middleware::from_fn(add_noindex_header));

    Router::new()
        .route("/", get(landing))
        .merge(crate::seo::routes())
        .merge(crate::auth::pages::asset_routes())
        .merge(non_indexable_routes)
        .fallback(not_found)
        .with_state(state)
        // Never log query strings: Google's callback query contains the provider code and state.
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &axum::http::Request<_>| {
                tracing::info_span!(
                    "http_request",
                    method = %request.method(),
                    path = %request.uri().path()
                )
            }),
        )
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER))
        .layer(SetRequestIdLayer::new(REQUEST_ID_HEADER, MakeRequestUuid))
}

async fn landing() -> Html<&'static str> {
    Html(include_str!("../../public/index.html"))
}

#[cfg(test)]
mod landing_page_tests {
    #[test]
    fn landing_page_is_product_specific_and_uses_verified_claim_boundaries() {
        let page = include_str!("../../public/index.html");
        for expected in [
            "<title>King Sparkon Eye | Face-Based Identity Verification Platform</title>",
            "Face-based identity verification.",
            "Face similarity is probabilistic",
            "integration-agnostic identity verification platform foundation",
            "tenant-isolated",
            "prefers-reduced-motion",
            "Skip to content",
            "README.md#current-http-surface",
            "Security &amp; privacy",
            "scope-title",
            "scope-caption",
            "href=\"/auth/sign-in\"",
            "href=\"/auth/register\"",
            "src=\"/assets/logo.gif\"",
            "src=\"/assets/hero.jpg\"",
            "class=\"brand-mark\"",
            "rel=\"canonical\"",
            "favicon.svg",
            "application/ld+json",
            "persistence: NOT_REQUESTED",
        ] {
            assert!(page.contains(expected), "landing page missing: {expected}");
        }
        assert!(!page.contains("event-scoped attendee identification"));
        assert!(!page.contains("Spring Boot"));
        assert!(!page.contains("openapi/eye.openapi.json"));
        assert!(!page.contains("Proposed API throughput limits"));
        assert!(!page.contains("99.9%"));
        assert!(!page.contains("trusted by"));
    }
}

#[cfg(test)]
mod billing_route_auth_tests {
    use super::*;
    use crate::auth::{AccessTokenClaims, UserRole, sign_access_token};
    use std::collections::HashMap;
    use tower::ServiceExt;

    fn config() -> Config {
        let values = HashMap::from([
            ("EYE_GOOGLE_CLIENT_ID", "client-id".to_owned()),
            ("EYE_GOOGLE_CLIENT_SECRET", "client-secret".to_owned()),
            (
                "EYE_GOOGLE_REDIRECT_URL",
                "https://eye.example.com/v1/auth/google/callback".to_owned(),
            ),
            (
                "EYE_DATABASE_URL",
                "postgres://eye:secret@db.example.com:5432/eye?sslmode=require".to_owned(),
            ),
            (
                "EYE_OAUTH_ENCRYPTION_KEY",
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_owned(),
            ),
            (
                "EYE_OAUTH_CALLBACK_URLS",
                "https://eye.example.com/auth/callback".to_owned(),
            ),
            ("EYE_OAUTH_EXCHANGE_SECRET", "e".repeat(48)),
            ("EYE_JWT_SIGNING_SECRET", "j".repeat(48)),
            ("EYE_JWT_KID", "billing-test-kid".to_owned()),
        ]);
        Config::from_lookup(|name| values.get(name).cloned()).expect("valid test config")
    }

    fn token(subject: &str, now: u64) -> String {
        let claims = AccessTokenClaims {
            iss: crate::auth::jwt::EYE_TOKEN_ISSUER.to_owned(),
            sub: subject.to_owned(),
            aud: crate::auth::jwt::EYE_TOKEN_AUDIENCE.to_owned(),
            exp: now + 600,
            iat: now,
            jti: format!("tok_{}", uuid::Uuid::new_v4().simple()),
            role: UserRole::GateOperator,
            scopes: Vec::new(),
            event_id: None,
        };
        sign_access_token(&claims, "billing-test-kid", "j".repeat(48).as_bytes())
            .expect("sign test token")
    }

    #[tokio::test]
    async fn billing_routes_reject_missing_auth_before_body_or_runtime_access() {
        let app = router(config());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/billing/checkout")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn verified_eye_subject_is_sufficient_without_billing_scope() {
        let now = current_unix_seconds();
        let bearer = token(&format!("usr_{}", "a".repeat(32)), now);
        let app = router(config());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/billing/checkout")
                    .header("authorization", format!("Bearer {bearer}"))
                    .header("idempotency-key", "checkout-request-1")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"plan":"gold","interval":"monthly","successUrl":"https://eye.example.com/billing/success","cancelUrl":"https://eye.example.com/billing/cancel"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        // Authentication succeeded; this test router intentionally has no billing runtime.
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn signed_but_non_eye_subject_is_rejected() {
        let now = current_unix_seconds();
        let bearer = token("tenant_123", now);
        let app = router(config());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/billing/portal")
                    .header("authorization", format!("Bearer {bearer}"))
                    .header("idempotency-key", "portal-request-1")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"returnUrl":"https://eye.example.com/account/billing"}"#,
                    ))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn billing_request_cannot_select_an_account_or_customer() {
        let now = current_unix_seconds();
        let bearer = token(&format!("usr_{}", "b".repeat(32)), now);
        let app = router(config());
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/billing/checkout")
                    .header("authorization", format!("Bearer {bearer}"))
                    .header("idempotency-key", "checkout-forged-owner")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"plan":"gold","interval":"monthly","successUrl":"https://eye.example.com/success","cancelUrl":"https://eye.example.com/cancel","eyeUserId":"usr_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","stripeCustomerId":"cus_attacker"}"#))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
