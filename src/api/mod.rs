//! HTTP routes: liveness, readiness and request-ID/tracing middleware.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderName, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::trace::TraceLayer;

use crate::config::Config;
use crate::error::ApiError;

pub const REQUEST_ID_HEADER: HeaderName = HeaderName::from_static("x-request-id");

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
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

async fn not_found() -> ApiError {
    ApiError::NotFound
}

pub fn router(config: Config) -> Router {
    let state = AppState {
        config: Arc::new(config),
    };
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .fallback(not_found)
        .with_state(state)
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::new(REQUEST_ID_HEADER))
        .layer(SetRequestIdLayer::new(REQUEST_ID_HEADER, MakeRequestUuid))
}
