//! HTTP routes: health probes, bounded photo ingress, OAuth, and request-ID/tracing middleware.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{header::{CONTENT_LENGTH, CONTENT_TYPE}, HeaderName, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::auth::oauth_runtime::OAuthRuntime;
use crate::config::Config;
use crate::contract::{PersistenceStatus, PhotoAssessmentResponse};
use crate::domain::{AssessmentConfig, assess, conservative_result};
use crate::inference::yunet::YunetDetector;
use crate::provider::DisabledProvider;
use crate::error::{ApiError, ValidationError};
use crate::ingress::ingest_with_timeout;

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

static YUNET_DETECTOR: OnceLock<Result<Arc<YunetDetector>, crate::inference::InferenceError>> = OnceLock::new();

fn load_detector() -> Result<Arc<YunetDetector>, crate::inference::InferenceError> {
    YUNET_DETECTOR.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let model_path = std::env::var_os("EYE_YUNET_MODEL_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("artifacts/models/yunet-2023mar/face_detection_yunet_2023mar.onnx"));
        let runtime_path = std::env::var_os("ORT_DYLIB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("artifacts/onnxruntime/1.31.1/linux-x86_64/libonnxruntime.so.1.31.1"));
        YunetDetector::load(&model_path, &runtime_path).map(Arc::new)
    }).clone()
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
const MULTIPART_MAX_HEADER_BYTES: usize = 8 * 1024;

async fn upload_photo(
    State(state): State<AppState>,
    request: Request<Body>,
) -> Result<Response, ApiError> {
    let request_id = request.headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let content_type = request
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::UnsupportedMediaType)?;
    let boundary = multipart_boundary(content_type)?;
    let body_limit = state
        .config
        .image_limits
        .max_upload_bytes
        .saturating_add(MULTIPART_OVERHEAD_BYTES);
    let declared_length = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    if declared_length.is_some_and(|length| length > body_limit) {
        return Err(ApiError::PayloadTooLarge);
    }
    let body = to_bytes(request.into_body(), body_limit)
        .await
        .map_err(|_| ApiError::PayloadTooLarge)?;
    let image = extract_multipart_image(
        &body,
        &boundary,
        state.config.image_limits.max_upload_bytes,
    )?;
    let normalized = ingest_with_timeout(image, state.config.image_limits)
        .await
        .map_err(ApiError::from)?;
    let runtime = state.assessment.as_ref().ok_or(ApiError::ServiceUnavailable)?;
    let disabled_provider = DisabledProvider;
    let result = assess(&normalized, runtime.detector.as_ref(), &disabled_provider, &AssessmentConfig::default()).await;
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

fn multipart_boundary(content_type: &str) -> Result<Vec<u8>, ApiError> {
    let mut parameters = content_type.split(';');
    if !parameters
        .next()
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("multipart/form-data"))
    {
        return Err(ApiError::UnsupportedMediaType);
    }
    let boundary = parameters
        .find_map(|parameter| {
            let (name, value) = parameter.trim().split_once('=')?;
            name.trim()
                .eq_ignore_ascii_case("boundary")
                .then_some(value.trim().trim_matches('"'))
        })
        .ok_or_else(invalid_multipart)?;
    let valid = !boundary.is_empty()
        && boundary.len() <= 70
        && boundary
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"'()+_,-./:=?".contains(&byte));
    if !valid {
        return Err(invalid_multipart());
    }
    Ok(boundary.as_bytes().to_vec())
}

fn extract_multipart_image(
    body: &[u8],
    boundary: &[u8],
    max_upload_bytes: usize,
) -> Result<Vec<u8>, ApiError> {
    let mut delimiter = b"--".to_vec();
    delimiter.extend_from_slice(boundary);
    if !body.starts_with(&delimiter) {
        return Err(invalid_multipart());
    }
    let mut cursor = delimiter.len();
    if body.get(cursor..cursor + 2) == Some(&b"--"[..]) {
        return Err(missing_image());
    }
    if body.get(cursor..cursor + 2) != Some(&b"\r\n"[..]) {
        return Err(invalid_multipart());
    }
    cursor += 2;
    let mut image = None;
    loop {
        let header_end = find_bytes(&body[cursor..], b"\r\n\r\n")
            .map(|offset| cursor + offset)
            .ok_or_else(invalid_multipart)?;
        if header_end - cursor > MULTIPART_MAX_HEADER_BYTES {
            return Err(invalid_multipart());
        }
        let data_start = header_end + 4;
        let mut separator = b"\r\n".to_vec();
        separator.extend_from_slice(&delimiter);
        let marker_start = find_bytes(&body[data_start..], &separator)
            .map(|offset| data_start + offset)
            .ok_or_else(invalid_multipart)?;
        let part_data = &body[data_start..marker_start];
        let field_name = multipart_field_name(&body[cursor..header_end])
            .ok_or_else(invalid_multipart)?;
        match field_name.as_str() {
            "image" => {
                if image.is_some() {
                    return Err(invalid_multipart());
                }
                if part_data.len() > max_upload_bytes {
                    return Err(ApiError::PayloadTooLarge);
                }
                image = Some(part_data.to_vec());
            }
            "clientRequestId" => {
                if part_data.len() > 1024 {
                    return Err(invalid_multipart());
                }
            }
            _ => return Err(invalid_multipart()),
        }
        let marker_after = marker_start + separator.len();
        let marker = body.get(marker_after..marker_after + 2);
        if marker == Some(&b"--"[..]) {
            let trailing = &body[marker_after + 2..];
            if !trailing.is_empty() && trailing != &b"\r\n"[..] {
                return Err(invalid_multipart());
            }
            break;
        } else if marker == Some(&b"\r\n"[..]) {
            cursor = marker_after + 2;
        } else {
            return Err(invalid_multipart());
        }
    }
    image.ok_or_else(missing_image)
}

fn multipart_field_name(headers: &[u8]) -> Option<String> {
    let headers = std::str::from_utf8(headers).ok()?;
    let disposition = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-disposition")
            .then_some(value.trim())
    })?;
    let mut parameters = disposition.split(';');
    if !parameters.next()?.trim().eq_ignore_ascii_case("form-data") {
        return None;
    }
    parameters.find_map(|parameter| {
        let (name, value) = parameter.trim().split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("name")
            .then(|| value.trim().trim_matches('"').to_owned())
    })
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
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
    let assessment = assessment_runtime();
    let state = AppState {
        config: Arc::new(config),
        oauth,
        assessment,
    };
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/v1/enrollment/photo-assessments", post(upload_photo))
        .merge(OAuthRuntime::routes())
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
