//! HTTP boundary for tenant-bound machine API credentials (#184).
//!
//! Human JWTs manage credentials; machine secrets authenticate protected API calls.
//! Tenant IDs in management paths are selectors only: every persistence operation
//! re-checks that the verified JWT subject owns the active tenant.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header::AUTHORIZATION},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::{
    auth::verify_access_token,
    config::Config,
    persistence::{
        self,
        api_credentials::{
            self, ApiCredentialError, AuthenticatedApiClient, CreateApiCredentialInput,
            CreatedApiCredential,
        },
        tenants::TenantId,
    },
};

#[derive(Clone)]
struct CredentialState {
    config: Arc<Config>,
    pool: PgPool,
}

pub fn router(config: Config) -> Router {
    let pool = config
        .oauth
        .as_ref()
        .and_then(|oauth| persistence::connect_lazy(&oauth.database_url).ok());
    let state = CredentialState {
        config: Arc::new(config),
        pool: pool.unwrap_or_else(|| {
            // Keep the routes mounted when storage is unavailable; handlers fail closed.
            // A deliberately invalid lazy URL is avoided: absence is represented separately.
            persistence::connect_lazy("postgres://invalid:invalid@127.0.0.1:1/invalid")
                .expect("static fallback PostgreSQL URL")
        }),
    };
    Router::new()
        .route("/v1/tenants/{tenant_id}/api-clients", post(create_client))
        .route(
            "/v1/tenants/{tenant_id}/api-clients/{client_id}/credentials",
            post(create_credential),
        )
        .route(
            "/v1/tenants/{tenant_id}/api-credentials/{credential_id}/rotate",
            post(rotate_credential),
        )
        .route(
            "/v1/tenants/{tenant_id}/api-credentials/{credential_id}",
            post(revoke_credential),
        )
        .route("/v1/machine/whoami", get(machine_whoami))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateClientRequest {
    display_name: String,
    scopes: Vec<String>,
    rate_limit_per_minute: i32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateCredentialRequest {
    scopes: Vec<String>,
    environment: String,
    ttl_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RotateCredentialRequest {
    environment: String,
    ttl_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ClientResponse {
    client_id: String,
    tenant_id: String,
    display_name: String,
    scopes: Vec<String>,
    rate_limit_per_minute: i32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CredentialResponse {
    credential_id: String,
    key_id: String,
    secret: String,
    scopes: Vec<String>,
    expires_at_ms: Option<i64>,
}

impl From<CreatedApiCredential> for CredentialResponse {
    fn from(value: CreatedApiCredential) -> Self {
        Self {
            credential_id: value.credential_id,
            key_id: value.key_id,
            secret: value.raw_secret,
            scopes: value.scopes,
            expires_at_ms: value.expires_at_ms,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MachineIdentityResponse {
    tenant_id: String,
    client_id: String,
    credential_id: String,
    owner_user_id: String,
    scopes: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    code: &'static str,
}

fn error(status: StatusCode, code: &'static str) -> Response {
    (status, Json(ErrorResponse { code })).into_response()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn human_actor(state: &CredentialState, headers: &HeaderMap) -> Result<String, Response> {
    let oauth = state.config.oauth.as_ref().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "credential_storage_unavailable",
        )
    })?;
    let secret = oauth.jwt_signing_secret.expose();
    if secret.len() < 32 {
        return Err(error(StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    let value = headers
        .get(AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let token = value
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, token)| token.trim())
        .filter(|token| !token.is_empty())
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let claims = verify_access_token(token, &oauth.jwt_kid, secret.as_bytes(), now_ms() / 1000)
        .map_err(|_| error(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    if !claims.sub.starts_with("usr_") {
        return Err(error(StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    Ok(claims.sub)
}

fn tenant_id(raw: &str) -> Result<TenantId, Response> {
    TenantId::parse(raw.to_owned()).map_err(|_| error(StatusCode::NOT_FOUND, "not_found"))
}

fn map_store_error(error_value: ApiCredentialError) -> Response {
    match error_value {
        ApiCredentialError::InvalidInput => error(StatusCode::BAD_REQUEST, "invalid_request"),
        ApiCredentialError::NotFound => error(StatusCode::NOT_FOUND, "not_found"),
        ApiCredentialError::InvalidCredential => error(StatusCode::UNAUTHORIZED, "unauthorized"),
        ApiCredentialError::InsufficientScope => error(StatusCode::FORBIDDEN, "insufficient_scope"),
        ApiCredentialError::RateLimited => error(StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        ApiCredentialError::Unavailable => {
            error(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable")
        }
    }
}

async fn create_client(
    State(state): State<CredentialState>,
    Path(raw_tenant): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CreateClientRequest>,
) -> Result<Json<ClientResponse>, Response> {
    let actor = human_actor(&state, &headers)?;
    let tenant = tenant_id(&raw_tenant)?;
    let client = api_credentials::create_api_client(
        &state.pool,
        &tenant,
        &actor,
        &request.display_name,
        request.scopes,
        request.rate_limit_per_minute,
        now_ms(),
    )
    .await
    .map_err(map_store_error)?;
    Ok(Json(ClientResponse {
        client_id: client.client_id,
        tenant_id: client.tenant_id.as_str().to_owned(),
        display_name: client.display_name,
        scopes: client.scopes,
        rate_limit_per_minute: client.rate_limit_per_minute,
    }))
}

async fn create_credential(
    State(state): State<CredentialState>,
    Path((raw_tenant, client_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<CreateCredentialRequest>,
) -> Result<Json<CredentialResponse>, Response> {
    let actor = human_actor(&state, &headers).map_err(map_human_error)?;
    let tenant = tenant_id(&raw_tenant).map_err(|status| error(status, "not_found"))?;
    let credential = api_credentials::create_api_credential(
        &state.pool,
        CreateApiCredentialInput {
            tenant_id: &tenant,
            client_id: &client_id,
            actor_user_id: &actor,
            environment: &request.environment,
            scopes: request.scopes,
            ttl_ms: request.ttl_ms,
            now_ms: now_ms(),
        },
    )
    .await
    .map_err(map_store_error)?;
    Ok(Json(credential.into()))
}

async fn rotate_credential(
    State(state): State<CredentialState>,
    Path((raw_tenant, credential_id)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<RotateCredentialRequest>,
) -> Result<Json<CredentialResponse>, Response> {
    let actor = human_actor(&state, &headers).map_err(map_human_error)?;
    let tenant = tenant_id(&raw_tenant).map_err(|status| error(status, "not_found"))?;
    let credential = api_credentials::rotate_api_credential(
        &state.pool,
        &tenant,
        &credential_id,
        &actor,
        &request.environment,
        request.ttl_ms,
        now_ms(),
    )
    .await
    .map_err(map_store_error)?;
    Ok(Json(credential.into()))
}

async fn revoke_credential(
    State(state): State<CredentialState>,
    Path((raw_tenant, credential_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, Response> {
    let actor = human_actor(&state, &headers).map_err(map_human_error)?;
    let tenant = tenant_id(&raw_tenant).map_err(|status| error(status, "not_found"))?;
    api_credentials::revoke_api_credential(&state.pool, &tenant, &credential_id, &actor, now_ms())
        .await
        .map_err(map_store_error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn machine_whoami(
    State(state): State<CredentialState>,
    headers: HeaderMap,
) -> Result<Json<MachineIdentityResponse>, Response> {
    let value = headers
        .get(AUTHORIZATION)
        .and_then(|header| header.to_str().ok())
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let secret = value
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, secret)| secret.trim())
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let principal: AuthenticatedApiClient = api_credentials::authenticate_api_credential(
        &state.pool,
        secret,
        Some("identities:read"),
        now_ms(),
    )
    .await
    .map_err(map_store_error)?;
    let client = sqlx::query_as::<_, (i32,)>(
        "SELECT rate_limit_per_minute FROM eye_api_clients WHERE tenant_id=$1 AND client_id=$2",
    )
    .bind(principal.tenant_id.as_str())
    .bind(&principal.client_id)
    .fetch_one(&state.pool)
    .await
    .map_err(|_| error(StatusCode::SERVICE_UNAVAILABLE, "storage_unavailable"))?;
    let allowed = api_credentials::consume_rate_limit(
        &state.pool,
        &principal.tenant_id,
        &principal.credential_id,
        now_ms(),
        client.0,
        client.0,
    )
    .await
    .map_err(map_store_error)?;
    if !allowed {
        return Err(error(StatusCode::TOO_MANY_REQUESTS, "rate_limited"));
    }
    Ok(Json(MachineIdentityResponse {
        tenant_id: principal.tenant_id.as_str().to_owned(),
        client_id: principal.client_id,
        credential_id: principal.credential_id,
        owner_user_id: principal.owner_user_id,
        scopes: principal.scopes,
    }))
}
