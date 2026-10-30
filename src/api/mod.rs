//! HTTP routes: liveness, readiness and request-ID/tracing middleware.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{header::CONTENT_TYPE, HeaderName, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::auth::oauth_runtime::OAuthRuntime;
use crate::config::Config;
use crate::error::{ApiError, ValidationError};
use crate::ingress::ingest_with_timeout;

pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub oauth: Option<Arc<OAuthRuntime>>,
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
    let report = readiness(&state.config);
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
    let content_type = request.headers().get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .ok_or(ApiError::UnsupportedMediaType)?;
    let boundary = multipart_boundary(content_type)?;
    let body_limit = state.config.image_limits.max_upload_bytes
        .saturating_add(MULTIPART_OVERHEAD_BYTES);
    let body = to_bytes(request.into_body(), body_limit)
        .await.map_err(|_| ApiError::PayloadTooLarge)?;
    let image = extract_multipart_image(
        &body, &boundary, state.config.image_limits.max_upload_bytes,
    )?;
    let _normalized = ingest_with_timeout(image, state.config.image_limits)
        .await.map_err(ApiError::from)?;
    Err(ApiError::AssessmentNotImplemented)
}

fn multipart_boundary(content_type: &str) -> Result<Vec<u8>, ApiError> {
    let mut parameters = content_type.split(';');
    if !parameters.next().is_some_and(|value|
        value.trim().eq_ignore_ascii_case("multipart/form-data")) {
        return Err(ApiError::UnsupportedMediaType);
    }
    let boundary = parameters.find_map(|parameter| {
        let (name, value) = parameter.trim().split_once('=')?;
        name.trim().eq_ignore_ascii_case("boundary")
            .then_some(value.trim().trim_matches('"'))
    }).ok_or_else(invalid_multipart)?;
    let valid = !boundary.is_empty() && boundary.len() <= 70
        && boundary.bytes().all(|byte|
            byte.is_ascii_alphanumeric() || b"'()+_,-./:=?".contains(&byte));
    if !valid { return Err(invalid_multipart()); }
    Ok(boundary.as_bytes().to_vec())
}

fn extract_multipart_image(
    body: &[u8], boundary: &[u8], max_upload_bytes: usize,
) -> Result<Vec<u8>, ApiError> {
    let mut delimiter = b"--".to_vec();
    delimiter.extend_from_slice(boundary);
    if !body.starts_with(&delimiter) { return Err(invalid_multipart()); }
    let mut cursor = delimiter.len();
    if body.get(cursor..cursor + 2) == Some(b"--") { return Err(missing_image()); }
    if body.get(cursor..cursor + 2) != Some(b"\r\n") {
        return Err(invalid_multipart());
    }
    cursor += 2;
    let mut image = None;
    loop {
        let header_end = find_bytes(&body[cursor..], b"\r\n\r\n")
            .map(|offset| cursor + offset).ok_or_else(invalid_multipart)?;
        if header_end - cursor > MULTIPART_MAX_HEADER_BYTES {
            return Err(invalid_multipart());
        }
        let data_start = header_end + 4;
        let mut separator = b"\r\n".to_vec();
        separator.extend_from_slice(&delimiter);
        let marker_start = find_bytes(&body[data_start..], &separator)
            .map(|offset| data_start + offset).ok_or_else(invalid_multipart)?;
        let part_data = &body[data_start..marker_start];
        let field_name = multipart_field_name(&body[cursor..header_end])
            .ok_or_else(invalid_multipart)?;
        match field_name.as_str() {
            "image" => {
                if image.is_some() { return Err(invalid_multipart()); }
                if part_data.len() > max_upload_bytes {
                    return Err(ApiError::PayloadTooLarge);
                }
                image = Some(part_data.to_vec());
            }
            "clientRequestId" => {
                if part_data.len() > 1024 { return Err(invalid_multipart()); }
            }
            _ => return Err(invalid_multipart()),
        }
        let marker_after = marker_start + separator.len();
        match body.get(marker_after..marker_after + 2) {
            Some(b"--") => {
                let trailing = &body[marker_after + 2..];
                if !trailing.is_empty() && trailing != b"\r\n" {
                    return Err(invalid_multipart());
                }
                break;
            }
            Some(b"\r\n") => cursor = marker_after + 2,
            _ => return Err(invalid_multipart()),
        }
    }
    image.ok_or_else(missing_image)
}

fn multipart_field_name(headers: &[u8]) -> Option<String> {
    let headers = std::str::from_utf8(headers).ok()?;
    let disposition = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("content-disposition")
            .then_some(value.trim())
    })?;
    let mut parameters = disposition.split(';');
    if !parameters.next()?.trim().eq_ignore_ascii_case("form-data") {
        return None;
    }
    parameters.find_map(|parameter| {
        let (name, value) = parameter.trim().split_once('=')?;
        name.trim().eq_ignore_ascii_case("name")
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
    }.into()
}

fn missing_image() -> ApiError {
    ValidationError { field: "image", rule: "is required" }.into()
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
    let state = AppState {
        config: Arc::new(config),
        oauth,
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
