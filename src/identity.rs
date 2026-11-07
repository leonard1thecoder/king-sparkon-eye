//! Machine-authenticated tenant-scoped identity registry HTTP routes (#185).
//!
//! The tenant is derived exclusively from the validated API credential. No tenant
//! selector is accepted in path, query, headers or request bodies.

use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header::{AUTHORIZATION, LOCATION}},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::PgPool;

use crate::{
    persistence::{self, api_credentials, identity_registry, verification, tenants::TenantId},
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
        .route("/v1/identities/{identity_id}/face-enrollments", post(create_enrollment_intent))
        .route("/v1/identities/{identity_id}/face-enrollments/{enrollment_id}", get(get_enrollment_intent))
        .route("/v1/identity-scopes", post(create_scope))
        .route("/v1/identity-scopes/{scope_id}/members/{identity_id}", put(add_scope_member).delete(remove_scope_member))
        .route("/v1/verification-sessions", post(create_verification_session))
        .route("/v1/verification-sessions/{session_id}", get(get_verification_session).delete(cancel_verification_session))
        .route("/v1/verification-sessions/{session_id}/captures", post(submit_verification_capture))
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

fn map_credential_error(error_value: api_credentials::ApiCredentialError) -> Response {
    use api_credentials::ApiCredentialError as E;
    match error_value {
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


#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateEnrollmentIntentRequest {
    consent_reference: String,
    consent_policy_version: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct EnrollmentIntentResponse {
    enrollment_id: String,
    identity_id: String,
    status: String,
    blocked_reason: Option<String>,
    consent_reference: String,
    consent_policy_version: String,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl From<identity_registry::IdentityEnrollment> for EnrollmentIntentResponse {
    fn from(value: identity_registry::IdentityEnrollment) -> Self {
        Self {
            enrollment_id: value.enrollment_id,
            identity_id: value.identity_id,
            status: value.status,
            blocked_reason: value.blocked_reason,
            consent_reference: value.consent_reference,
            consent_policy_version: value.consent_policy_version,
            created_at_ms: value.created_at_ms,
            updated_at_ms: value.updated_at_ms,
        }
    }
}

fn idempotency_key(headers: &HeaderMap) -> Result<&str, Response> {
    let value = headers.get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "idempotency_key_required"))?;
    if value.is_empty() || value.len() > 128
        || !value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(error(StatusCode::BAD_REQUEST, "invalid_idempotency_key"));
    }
    Ok(value)
}

async fn create_enrollment_intent(
    State(state): State<IdentityState>,
    Path(identity_id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<CreateEnrollmentIntentRequest>,
) -> Result<(StatusCode, [(axum::http::HeaderName, axum::http::HeaderValue); 1], Json<EnrollmentIntentResponse>), Response> {
    let principal = principal(&state, &headers, "identities:write").await?;
    let key = idempotency_key(&headers)?;
    let outcome = identity_registry::create_enrollment_intent(
        &state.pool,
        &principal.tenant_id,
        &identity_id,
        &principal.client_id,
        &principal.owner_user_id,
        &request.consent_reference,
        &request.consent_policy_version,
        key,
        now_ms(),
    ).await.map_err(map_identity_error)?;

    let (status, enrollment) = match outcome {
        identity_registry::EnrollmentIntentOutcome::Created(value) => (StatusCode::ACCEPTED, value),
        identity_registry::EnrollmentIntentOutcome::Existing(value) => (StatusCode::OK, value),
    };
    let location = format!("/v1/identities/{identity_id}/face-enrollments/{}", enrollment.enrollment_id);
    let location = axum::http::HeaderValue::from_str(&location)
        .map_err(|_| error(StatusCode::INTERNAL_SERVER_ERROR, "internal_error"))?;
    Ok((status, [(LOCATION, location)], Json(enrollment.into())))
}

async fn get_enrollment_intent(
    State(state): State<IdentityState>,
    Path((identity_id, enrollment_id)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<EnrollmentIntentResponse>, Response> {
    let principal = principal(&state, &headers, "identities:read").await?;
    let enrollment = identity_registry::get_enrollment_intent(
        &state.pool, &principal.tenant_id, &identity_id, &enrollment_id,
    ).await.map_err(map_identity_error)?;
    Ok(Json(enrollment.into()))
}

async fn create_identity(
    State(state): State<IdentityState>,
    headers: HeaderMap,
    Json(request): Json<CreateIdentityRequest>,
) -> Result<(StatusCode, Json<IdentityResponse>), Response> {
    let principal = principal(&state, &headers, "identities:write").await?;
    let tenant = TenantId::parse(principal.tenant_id.as_str()).map_err(|_| error(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let identity = identity_registry::create_identity(
        &state.pool, &tenant, &request.external_reference, request.metadata,
        &principal.client_id, &principal.owner_user_id, now_ms(),
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

#[derive(Debug, Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct CreateScopeRequest {
    external_reference:String,
    display_name:String,
    description:Option<String>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all="camelCase")]
struct ScopeResponse {
    scope_id:String, external_reference:String, display_name:String,
    description:Option<String>, status:String, member_count:i64,
    created_at_ms:i64, updated_at_ms:i64,
}
impl From<verification::IdentityScope> for ScopeResponse {
    fn from(v:verification::IdentityScope)->Self { Self {
        scope_id:v.scope_id,external_reference:v.external_reference,display_name:v.display_name,
        description:v.description,status:v.status,member_count:v.member_count,
        created_at_ms:v.created_at_ms,updated_at_ms:v.updated_at_ms
    }}
}
fn map_verification_error(e:verification::VerificationStoreError)->Response {
    use verification::VerificationStoreError as E;
    match e {
        E::InvalidInput=>error(StatusCode::BAD_REQUEST,"invalid_request"),
        E::NotFound=>error(StatusCode::NOT_FOUND,"not_found"),
        E::Conflict=>error(StatusCode::CONFLICT,"idempotency_conflict"),
        E::Unavailable=>error(StatusCode::SERVICE_UNAVAILABLE,"storage_unavailable"),
    }
}
async fn create_scope(
    State(state):State<IdentityState>, headers:HeaderMap, Json(req):Json<CreateScopeRequest>
)->Result<(StatusCode,Json<ScopeResponse>),Response>{
    let p=principal(&state,&headers,"identity_scopes:write").await?;
    let tenant=TenantId::parse(p.tenant_id.as_str()).map_err(|_|error(StatusCode::UNAUTHORIZED,"unauthorized"))?;
    let scope=verification::VerificationStore::new(state.pool.clone())
        .create_scope(&tenant,&req.external_reference,&req.display_name,req.description.as_deref(),now_ms())
        .await.map_err(map_verification_error)?;
    Ok((StatusCode::CREATED,Json(scope.into())))
}
async fn add_scope_member(
    State(state):State<IdentityState>,Path((scope_id,identity_id)):Path<(String,String)>,headers:HeaderMap
)->Result<StatusCode,Response>{
    let p=principal(&state,&headers,"identity_scopes:write").await?;
    let tenant=TenantId::parse(p.tenant_id.as_str()).map_err(|_|error(StatusCode::UNAUTHORIZED,"unauthorized"))?;
    verification::VerificationStore::new(state.pool.clone()).add_member(&tenant,&scope_id,&identity_id,now_ms())
        .await.map_err(map_verification_error)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn remove_scope_member(
    State(state):State<IdentityState>,Path((scope_id,identity_id)):Path<(String,String)>,headers:HeaderMap
)->Result<StatusCode,Response>{
    let p=principal(&state,&headers,"identity_scopes:write").await?;
    let tenant=TenantId::parse(p.tenant_id.as_str()).map_err(|_|error(StatusCode::UNAUTHORIZED,"unauthorized"))?;
    verification::VerificationStore::new(state.pool.clone()).remove_member(&tenant,&scope_id,&identity_id,now_ms())
        .await.map_err(map_verification_error)?;
    Ok(StatusCode::NO_CONTENT)
}

fn default_session_ttl()->i64{300_000}
fn empty_context()->Value{serde_json::json!({})}
#[derive(Debug,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct CreateVerificationSessionRequest {
    mode:String,
    identity_id:Option<String>,
    scope_id:Option<String>,
    purpose:Option<String>,
    #[serde(default="empty_context")]
    context:Value,
    #[serde(default="default_session_ttl")]
    expires_in_ms:i64,
}
#[derive(Debug,Serialize)]
#[serde(rename_all="camelCase")]
struct VerificationSessionResponse {
    session_id:String, mode:String, identity_id:Option<String>, scope_id:Option<String>,
    status:String, outcome:Option<String>, reason_code:Option<String>, purpose:Option<String>,
    context:Value, expires_at_ms:i64, created_at_ms:i64, updated_at_ms:i64,
    completed_at_ms:Option<i64>,
}
impl From<verification::VerificationSession> for VerificationSessionResponse {
    fn from(v:verification::VerificationSession)->Self{Self{
        session_id:v.session_id,mode:v.mode,identity_id:v.identity_id,scope_id:v.scope_id,
        status:v.status,outcome:v.outcome,reason_code:v.reason_code,purpose:v.purpose,
        context:v.context,expires_at_ms:v.expires_at_ms,created_at_ms:v.created_at_ms,
        updated_at_ms:v.updated_at_ms,completed_at_ms:v.completed_at_ms
    }}
}
async fn create_verification_session(
    State(state):State<IdentityState>,headers:HeaderMap,Json(req):Json<CreateVerificationSessionRequest>
)->Result<(StatusCode,[(axum::http::HeaderName,axum::http::HeaderValue);1],Json<VerificationSessionResponse>),Response>{
    let p=principal(&state,&headers,"verification:write").await?;
    let key=idempotency_key(&headers)?;
    let tenant=TenantId::parse(p.tenant_id.as_str()).map_err(|_|error(StatusCode::UNAUTHORIZED,"unauthorized"))?;
    let (created,session)=verification::VerificationStore::new(state.pool.clone()).create_session(
        &tenant,&p.client_id,&req.mode,req.identity_id.as_deref(),req.scope_id.as_deref(),
        req.purpose.as_deref(),req.context,key,now_ms(),req.expires_in_ms
    ).await.map_err(map_verification_error)?;
    let location=axum::http::HeaderValue::from_str(&format!("/v1/verification-sessions/{}",session.session_id))
        .map_err(|_|error(StatusCode::INTERNAL_SERVER_ERROR,"internal_error"))?;
    Ok((if created{StatusCode::CREATED}else{StatusCode::OK},[(LOCATION,location)],Json(session.into())))
}
async fn get_verification_session(
    State(state):State<IdentityState>,Path(session_id):Path<String>,headers:HeaderMap
)->Result<Json<VerificationSessionResponse>,Response>{
    let p=principal(&state,&headers,"verification:read").await?;
    let tenant=TenantId::parse(p.tenant_id.as_str()).map_err(|_|error(StatusCode::UNAUTHORIZED,"unauthorized"))?;
    let session=verification::VerificationStore::new(state.pool.clone()).get_session(&tenant,&session_id,now_ms())
        .await.map_err(map_verification_error)?;
    Ok(Json(session.into()))
}
async fn cancel_verification_session(
    State(state):State<IdentityState>,Path(session_id):Path<String>,headers:HeaderMap
)->Result<Json<VerificationSessionResponse>,Response>{
    let p=principal(&state,&headers,"verification:write").await?;
    let tenant=TenantId::parse(p.tenant_id.as_str()).map_err(|_|error(StatusCode::UNAUTHORIZED,"unauthorized"))?;
    let session=verification::VerificationStore::new(state.pool.clone()).cancel_session(&tenant,&session_id,now_ms())
        .await.map_err(map_verification_error)?;
    Ok(Json(session.into()))
}
/// Capture ingress is deliberately closed while #95 lacks its consented evaluation
/// dataset and approved operating thresholds. This route does not parse or persist bytes.
async fn submit_verification_capture(
    State(state):State<IdentityState>,Path(_session_id):Path<String>,headers:HeaderMap
)->Result<Response,Response>{
    let p=principal(&state,&headers,"verification:write").await?;
    let _key=idempotency_key(&headers)?;
    let tenant=TenantId::parse(p.tenant_id.as_str()).map_err(|_|error(StatusCode::UNAUTHORIZED,"unauthorized"))?;
    let session=verification::VerificationStore::new(state.pool.clone())
        .get_session(&tenant,&_session_id,now_ms()).await.map_err(map_verification_error)?;
    if !matches!(session.status.as_str(),"AWAITING_CAPTURE"|"BLOCKED_EVALUATION") {
        return Err(error(StatusCode::CONFLICT,"session_not_accepting_captures"));
    }
    Err(error(StatusCode::SERVICE_UNAVAILABLE,"evaluation_gate_closed"))
}
