//! Machine-authenticated tenant-scoped identity registry HTTP routes (#185).
//!
//! The tenant is derived exclusively from the validated API credential. No tenant
//! selector is accepted in path, query, headers or request bodies.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;

use crate::{
    persistence::{self, api_credentials, identity_registry, tenants::TenantId},
    config::Config,
};

#[derive(Clone)]
struct IdentityState {
    pool: PgPool,
}

pub fn router(config: Config) -> Router {
    let pool = config.oauth.as_ref()
        .and_then(|oauth| persistence::connect_lazy(&oauth.database_url).ok())
        .unwrap_or_else(|| persistence::connect_lazy("postgres://invalid:invalid@127.0.0.1:1/invalid").expect("static fallback PostgreSQL URL"));
    Router::new()
        .route("/v1/identities", post(create_identity))
        .route("/v1/identities/{identity_id}", get(get_identity).delete(delete_identity))
        .route("/v1/identities/{identity_id}/revoke", post(revoke_identity))
        .with_state(IdentityState { pool })
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateIdentityRequest {
    external_reference: String,
    #[serde(default = "empty_metadata")]
    metadata: Value,
}

fn empty_metadata() -> Value { serde_json::json!({}) }

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct IdentityResponse {
    identity_id: String,
    external_reference: String,
    status: String,
    enrollment_status: String,
    metadata: Value,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl From<identity_registry::Identity> for IdentityResponse {
    fn from(value: identity_registry::Identity) -> Self {
        Self {
            identity_id: value.identity_id,
            external_reference: value.external_reference,
            status: value.status,
            enrollment_status: value.enrollment_status,
            metadata: value.metadata,
            created_at_ms: value.created_at_ms,
            updated_at_ms: value.updated_at_ms,
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorBody { code: &'static str }

fn error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(ErrorBody { code })).into_response()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64).unwrap_or(0)
}

async fn principal(
    state: &IdentityState,
    headers: &HeaderMap,
    required_scope: &'static str,
) -> Result<api_credentials::AuthenticatedApiClient, Response> {
    let value = headers.get(AUTHORIZATION).and_then(|h| h.to_str().ok())
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let secret = value.split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, secret)| secret.trim())
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let principal = api_credentials::authenticate_api_credential(
        &state.pool, secret, Some(required_scope), now_ms().max(0) as u64,
    ).await.map_err(map_credential_error)?;

    let limit: i32 = sqlx::query_scalar(
        "SELECT rate_limit_per_minute FROM eye_api_clients WHERE tenant_id=$1 AND client_id=$2 AND status='ACTIVE'",
    ).bind(principal.tenant_id.as_str()).bind(&principal.client_id)
        .fetch_optional(&state.pool).await
        .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"))?
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let allowed = api_credentials::consume_rate_limit(
        &state.pool, &principal.tenant_id, &principal.credential_id,
        now_ms().max(0) as u64, limit, limit,
    ).await.map_err(map_credential_error)?;
    if !allowed { return Err(error(StatusCode::TOO_MANY_REQUESTS, "rate_limited")); }
    Ok(principal)
}

fn map_credential_error(error: api_credentials::ApiCredentialError) -> Response {
    use api_credentials::ApiCredentialError as E;
    match error {
        E::InvalidInput => error(StatusCode::BAD_REQUEST, "invalid_request"),
        E::NotFound => error(StatusCode::NOT_FOUND, "not_found"),
        E::InvalidCredential => error(StatusCode::UNAUTHORIZED, "unauthorized"),
        E::InsufficientScope => error(StatusCode::FORBIDDEN, "insufficient_scope"),
        E::RateLimited => error(StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        E::Unavailable => error(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    }
}

fn map_identity_error(error_value: identity_registry::IdentityStoreError) -> Response {
    use identity_registry::IdentityStoreError as E;
    match error_value {
        E::InvalidInput => error(StatusCode::BAD_REQUEST, "invalid_request"),
        E::NotFound => error(StatusCode::NOT_FOUND, "not_found"),
        E::Conflict => error(StatusCode::CONFLICT, "external_reference_conflict"),
        E::Unavailable => error(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"),
    }
}

async fn create_identity(
    State(state): State<IdentityState>,
    headers: HeaderMap,
    Json(request): Json<CreateIdentityRequest>,
) -> Result<(StatusCode, Json<IdentityResponse>), Response> {
    let principal = principal(&state, &headers, "identities:write").await?;
    let tenant = TenantId::parse(principal.tenant_id.as_str()).map_err(|_| error(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let identity = identity_registry::create_identity(
        &state.pool, &tenant, &request.external_reference, request.metadata, now_ms(),
    ).await.map_err(map_identity_error)?;
    Ok((StatusCode::CREATED, Json(identity.into())))
}

async fn get_identity(
    State(state): State<IdentityState>,
    Path(identity_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<IdentityResponse>, Response> {
    let principal = principal(&state, &headers, "identities:read").await?;
    let identity = identity_registry::get_identity(&state.pool, &principal.tenant_id, &identity_id)
        .await.map_err(map_identity_error)?;
    Ok(Json(identity.into()))
}

async fn revoke_identity(
    State(state): State<IdentityState>,
    Path(identity_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<IdentityResponse>, Response> {
    let principal = principal(&state, &headers, "identities:write").await?;
    let identity = identity_registry::revoke_identity(
        &state.pool, &principal.tenant_id, &identity_id, &principal.client_id,
        &principal.owner_user_id, now_ms(),
    ).await.map_err(map_identity_error)?;
    Ok(Json(identity.into()))
}

async fn delete_identity(
    State(state): State<IdentityState>,
    Path(identity_id): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, Response> {
    let principal = principal(&state, &headers, "identities:write").await?;
    identity_registry::delete_identity(
        &state.pool, &principal.tenant_id, &identity_id, &principal.client_id,
        &principal.owner_user_id, now_ms(),
    ).await.map_err(map_identity_error)?;
    Ok(StatusCode::NO_CONTENT)
}
