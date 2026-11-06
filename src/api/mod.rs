//! HTTP routes: health probes, bounded photo ingress, OAuth, and request-ID/tracing middleware.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, StatusCode, header::AUTHORIZATION};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::auth::oauth_runtime::OAuthRuntime;
use crate::auth::{BoundaryError, CallerIdentity, RouteGuard, verify_access_token};
use crate::config::Config;
use crate::contract::{PersistenceStatus, PhotoAssessmentResponse};
use crate::domain::{AssessmentConfig, assess, conservative_result};
use crate::error::{ApiError, ValidationError};
use crate::inference::yunet::YunetDetector;
use crate::ingress::ingest_with_timeout;
use crate::provider::DisabledProvider;

pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub oauth: Option<Arc<OAuthRuntime>>,
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
        .map_err(|_| ApiError::UnsupportedMediaType)?;
    let mut image = None;
    while let Some(mut field) = multipart
        .next_field()
        .await
        .map_err(|_| invalid_multipart())?
    {
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

pub fn router(config: Config) -> Router {
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
        assessment,
    };
    let non_indexable_routes = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route(
            "/v1/enrollment/photo-assessments",
            post(upload_photo).layer(DefaultBodyLimit::max(upload_body_limit)),
        )
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
