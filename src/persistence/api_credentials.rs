//! Tenant-bound machine API clients and credentials (#184).
//!
//! Human JWT sessions are deliberately not accepted here. A verified API key resolves
//! the tenant from its persisted credential row; request bodies, query parameters and
//! headers never select tenant authority.

use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::auth::hash_api_key;
use crate::persistence::tenants::TenantId;

const ALLOWED_SCOPES: &[&str] = &[
    "identities:read",
    "identities:write",
    "scopes:read",
    "scopes:write",
    "verification:create",
    "verification:read",
    "evidence:read",
    "webhooks:manage",
];
const API_KEY_PREFIX: &str = "kse_";
const KEY_ID_RANDOM_LEN: usize = 12;
const SECRET_HEX_LEN: usize = 64;

fn valid_environment(environment: &str) -> bool {
    matches!(environment, "dev" | "test" | "live")
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiCredentialError {
    #[error("input is invalid")]
    InvalidInput,
    #[error("tenant or owner is not authorized")]
    NotFound,
    #[error("credential is invalid, expired, or revoked")]
    InvalidCredential,
    #[error("credential does not grant the required permission")]
    InsufficientScope,
    #[error("rate limit exceeded")]
    RateLimited,
    #[error("persistence is unavailable")]
    Unavailable,
}

impl From<sqlx::Error> for ApiCredentialError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiClient {
    pub client_id: String,
    pub tenant_id: TenantId,
    pub owner_user_id: String,
    pub display_name: String,
    pub scopes: Vec<String>,
    pub rate_limit_per_minute: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedApiClient {
    pub tenant_id: TenantId,
    pub client_id: String,
    pub credential_id: String,
    pub owner_user_id: String,
    pub scopes: Vec<String>,
}

#[derive(Clone, PartialEq, Eq)]
pub struct CreatedApiCredential {
    pub credential_id: String,
    pub key_id: String,
    pub raw_secret: String,
    pub scopes: Vec<String>,
    pub expires_at_ms: Option<i64>,
}

impl std::fmt::Debug for CreatedApiCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedApiCredential")
            .field("credential_id", &self.credential_id)
            .field("key_id", &self.key_id)
            .field("raw_secret", &"[REDACTED]")
            .field("scopes", &self.scopes)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

fn validate_scopes(scopes: &[String]) -> Result<(), ApiCredentialError> {
    if scopes.is_empty()
        || scopes
            .iter()
            .any(|scope| !ALLOWED_SCOPES.contains(&scope.as_str()))
        || scopes
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != scopes.len()
    {
        return Err(ApiCredentialError::InvalidInput);
    }
    Ok(())
}

fn generate_secret(environment: &str) -> Result<(String, String, String, String), ApiCredentialError> {
    if !valid_environment(environment) {
        return Err(ApiCredentialError::InvalidInput);
    }
    let random = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let raw_secret = format!("{API_KEY_PREFIX}{environment}_{random}");
    let key_id = format!("{API_KEY_PREFIX}{environment}_{}", &random[..KEY_ID_RANDOM_LEN]);
    let salt = Uuid::new_v4().simple().to_string();
    let hash = hash_api_key(&raw_secret, &salt);
    Ok((raw_secret, key_id, salt, hash))
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |diff, (a, b)| diff | (a ^ b))
        == 0
}

fn to_i64(value: u64) -> Result<i64, ApiCredentialError> {
    i64::try_from(value).map_err(|_| ApiCredentialError::InvalidInput)
}

fn validate_name(name: &str) -> Result<(), ApiCredentialError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 120 || name.chars().any(char::is_control) {
        return Err(ApiCredentialError::InvalidInput);
    }
    Ok(())
}

/// Create a machine client for an active tenant owned by the verified human actor.
pub async fn create_api_client(
    pool: &PgPool,
    tenant_id: &TenantId,
    owner_user_id: &str,
    display_name: &str,
    scopes: Vec<String>,
    rate_limit_per_minute: i32,
    now_ms: u64,
) -> Result<ApiClient, ApiCredentialError> {
    validate_name(display_name)?;
    validate_scopes(&scopes)?;
    if rate_limit_per_minute <= 0 || rate_limit_per_minute > 100_000 {
        return Err(ApiCredentialError::InvalidInput);
    }
    let now_ms = to_i64(now_ms)?;
    let client_id = format!("cli_{}", Uuid::new_v4().simple());
    let name = display_name.trim();

    let row = sqlx::query(
        "INSERT INTO eye_api_clients (client_id, tenant_id, owner_user_id, display_name, scopes, status, rate_limit_per_minute, created_at_ms, updated_at_ms)          SELECT $1, t.tenant_id, t.owner_user_id, $3, $4, 'ACTIVE', $5, $6, $6          FROM eye_tenants t JOIN eye_auth_users u ON u.user_id = t.owner_user_id          WHERE t.tenant_id = $2 AND t.owner_user_id = $7 AND t.status = 'ACTIVE' AND u.is_active = TRUE          RETURNING client_id, tenant_id, owner_user_id, display_name, scopes, rate_limit_per_minute",
    )
    .bind(&client_id)
    .bind(tenant_id.as_str())
    .bind(name)
    .bind(&scopes)
    .bind(rate_limit_per_minute)
    .bind(now_ms)
    .bind(owner_user_id)
    .fetch_optional(pool)
    .await?
    .ok_or(ApiCredentialError::NotFound)?;

    Ok(ApiClient {
        client_id: row.try_get("client_id")?,
        tenant_id: TenantId::parse(row.try_get::<String, _>("tenant_id")?)
            .map_err(|_| ApiCredentialError::Unavailable)?,
        owner_user_id: row.try_get("owner_user_id")?,
        display_name: row.try_get("display_name")?,
        scopes: row.try_get("scopes")?,
        rate_limit_per_minute: row.try_get("rate_limit_per_minute")?,
    })
}

async fn insert_credential(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: &TenantId,
    client_id: &str,
    actor_user_id: &str,
    scopes: &[String],
    expires_at_ms: Option<i64>,
    now_ms: i64,
    rotated_from: Option<&str>,
    environment: &str,
) -> Result<CreatedApiCredential, ApiCredentialError> {
    let (raw_secret, key_id, salt, hash) = generate_secret(environment)?;
    let credential_id = format!("crd_{}", Uuid::new_v4().simple());

    sqlx::query(
        "INSERT INTO eye_api_credentials (credential_id, tenant_id, client_id, key_id, key_hash, salt, scopes, expires_at_ms, created_at_ms, rotated_from_credential_id)          VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(&credential_id)
    .bind(tenant_id.as_str())
    .bind(client_id)
    .bind(&key_id)
    .bind(&hash)
    .bind(&salt)
    .bind(scopes)
    .bind(expires_at_ms)
    .bind(now_ms)
    .bind(rotated_from)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "INSERT INTO eye_api_credential_audit (tenant_id, client_id, credential_id, actor_user_id, action, created_at_ms)          VALUES ($1,$2,$3,$4,'CREDENTIAL_CREATED',$5)",
    )
    .bind(tenant_id.as_str())
    .bind(client_id)
    .bind(&credential_id)
    .bind(actor_user_id)
    .bind(now_ms)
    .execute(&mut **tx)
    .await?;

    Ok(CreatedApiCredential {
        credential_id,
        key_id,
        raw_secret,
        scopes: scopes.to_vec(),
        expires_at_ms,
    })
}

/// Create a credential. The secret is returned once and never persisted.
pub async fn create_api_credential(
    pool: &PgPool,
    tenant_id: &TenantId,
    client_id: &str,
    actor_user_id: &str,
    environment: &str,
    scopes: Vec<String>,
    ttl_ms: Option<u64>,
    now_ms: u64,
) -> Result<CreatedApiCredential, ApiCredentialError> {
    validate_scopes(&scopes)?;
    if !valid_environment(environment) {
        return Err(ApiCredentialError::InvalidInput);
    }
    let now_ms_i64 = to_i64(now_ms)?;
    let expires_at_ms = ttl_ms
        .map(|ttl| {
            now_ms
                .checked_add(ttl)
                .ok_or(ApiCredentialError::InvalidInput)
        })
        .transpose()?
        .map(to_i64)
        .transpose()?;

    let mut tx = pool.begin().await?;
    let client_scopes: Option<Vec<String>> = sqlx::query_scalar(
        "SELECT scopes FROM eye_api_clients          WHERE tenant_id=$1 AND client_id=$2 AND owner_user_id=$3 AND status='ACTIVE'          FOR UPDATE",
    )
    .bind(tenant_id.as_str())
    .bind(client_id)
    .bind(actor_user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let client_scopes = client_scopes.ok_or(ApiCredentialError::NotFound)?;
    if scopes.iter().any(|scope| !client_scopes.contains(scope)) {
        return Err(ApiCredentialError::InsufficientScope);
    }

    let created = insert_credential(
        &mut tx,
        tenant_id,
        client_id,
        actor_user_id,
        &scopes,
        expires_at_ms,
        now_ms_i64,
        None,
        environment,
    )
    .await?;
    tx.commit().await?;
    Ok(created)
}

/// Resolve a machine credential to its persisted tenant and enforce its granted scope.
pub async fn authenticate_api_credential(
    pool: &PgPool,
    raw_secret: &str,
    required_scope: Option<&str>,
    now_ms: u64,
) -> Result<AuthenticatedApiClient, ApiCredentialError> {
    let mut parts = raw_secret.split('_');
    if parts.next() != Some("kse") {
        return Err(ApiCredentialError::InvalidCredential);
    }
    let environment = parts.next().ok_or(ApiCredentialError::InvalidCredential)?;
    let random = parts.next().ok_or(ApiCredentialError::InvalidCredential)?;
    if parts.next().is_some()
        || !valid_environment(environment)
        || random.len() != SECRET_HEX_LEN
        || !random.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(ApiCredentialError::InvalidCredential);
    }
    let key_id = format!("{API_KEY_PREFIX}{environment}_{}", &random[..KEY_ID_RANDOM_LEN]);
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT c.credential_id, c.tenant_id, c.client_id, c.key_hash, c.salt, c.scopes, \
                a.owner_user_id, a.scopes AS client_scopes \
         FROM eye_api_credentials c \
         JOIN eye_api_clients a ON a.tenant_id=c.tenant_id AND a.client_id=c.client_id \
         JOIN eye_tenants t ON t.tenant_id=c.tenant_id \
         JOIN eye_auth_users u ON u.user_id=a.owner_user_id \
         WHERE c.key_id=$1 AND c.revoked_at_ms IS NULL \
           AND (c.expires_at_ms IS NULL OR c.expires_at_ms > $2) \
           AND a.status='ACTIVE' AND t.status='ACTIVE' AND u.is_active=TRUE \
         FOR UPDATE OF c",
    )
    .bind(&key_id)
    .bind(to_i64(now_ms)?)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiCredentialError::InvalidCredential)?;

    let stored_hash: String = row.try_get("key_hash")?;
    let salt: String = row.try_get("salt")?;
    let candidate = hash_api_key(raw_secret, &salt);
    if !constant_time_eq(candidate.as_bytes(), stored_hash.as_bytes()) {
        return Err(ApiCredentialError::InvalidCredential);
    }

    let scopes: Vec<String> = row.try_get("scopes")?;
    let client_scopes: Vec<String> = row.try_get("client_scopes")?;
    if scopes.iter().any(|scope| !client_scopes.contains(scope)) {
        return Err(ApiCredentialError::InvalidCredential);
    }
    if let Some(required) = required_scope
        && !scopes.iter().any(|scope| scope == required)
    {
        return Err(ApiCredentialError::InsufficientScope);
    }

    let credential_id: String = row.try_get("credential_id")?;
    let tenant_id = TenantId::parse(row.try_get::<String, _>("tenant_id")?)
        .map_err(|_| ApiCredentialError::Unavailable)?;
    let client_id: String = row.try_get("client_id")?;
    let owner_user_id: String = row.try_get("owner_user_id")?;

    sqlx::query(
        "UPDATE eye_api_credentials SET last_used_at_ms=$2 WHERE credential_id=$1 AND last_used_at_ms IS DISTINCT FROM $2",
    )
    .bind(&credential_id)
    .bind(to_i64(now_ms)?)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(AuthenticatedApiClient {
        tenant_id,
        client_id,
        credential_id,
        owner_user_id,
        scopes,
    })
}

/// Revoke one credential. Ownership is checked against the authenticated human actor.
pub async fn revoke_api_credential(
    pool: &PgPool,
    tenant_id: &TenantId,
    credential_id: &str,
    actor_user_id: &str,
    now_ms: u64,
) -> Result<(), ApiCredentialError> {
    let now_ms = to_i64(now_ms)?;
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "UPDATE eye_api_credentials c SET revoked_at_ms=$4          FROM eye_api_clients a          WHERE c.tenant_id=$1 AND c.credential_id=$2 AND c.client_id=a.client_id            AND a.tenant_id=c.tenant_id AND a.owner_user_id=$3 AND c.revoked_at_ms IS NULL          RETURNING c.client_id",
    )
    .bind(tenant_id.as_str())
    .bind(credential_id)
    .bind(actor_user_id)
    .bind(now_ms)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiCredentialError::NotFound)?;
    let client_id: String = row.try_get("client_id")?;
    sqlx::query(
        "INSERT INTO eye_api_credential_audit (tenant_id, client_id, credential_id, actor_user_id, action, created_at_ms)          VALUES ($1,$2,$3,$4,'CREDENTIAL_REVOKED',$5)",
    )
    .bind(tenant_id.as_str())
    .bind(client_id)
    .bind(credential_id)
    .bind(actor_user_id)
    .bind(now_ms)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Rotate atomically: revoke the old secret and create its replacement in one transaction.
pub async fn rotate_api_credential(
    pool: &PgPool,
    tenant_id: &TenantId,
    credential_id: &str,
    actor_user_id: &str,
    environment: &str,
    ttl_ms: Option<u64>,
    now_ms: u64,
) -> Result<CreatedApiCredential, ApiCredentialError> {
    if !valid_environment(environment) {
        return Err(ApiCredentialError::InvalidInput);
    }
    let now_ms_i64 = to_i64(now_ms)?;
    let expires_at_ms = ttl_ms
        .map(|ttl| {
            now_ms
                .checked_add(ttl)
                .ok_or(ApiCredentialError::InvalidInput)
        })
        .transpose()?
        .map(to_i64)
        .transpose()?;
    let mut tx = pool.begin().await?;
    let row = sqlx::query(
        "SELECT c.client_id, c.scopes FROM eye_api_credentials c          JOIN eye_api_clients a ON a.tenant_id=c.tenant_id AND a.client_id=c.client_id          WHERE c.tenant_id=$1 AND c.credential_id=$2 AND a.owner_user_id=$3            AND a.status='ACTIVE' AND c.revoked_at_ms IS NULL            AND (c.expires_at_ms IS NULL OR c.expires_at_ms > $4) FOR UPDATE OF c",
    )
    .bind(tenant_id.as_str())
    .bind(credential_id)
    .bind(actor_user_id)
    .bind(now_ms_i64)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(ApiCredentialError::NotFound)?;
    let client_id: String = row.try_get("client_id")?;
    let scopes: Vec<String> = row.try_get("scopes")?;

    sqlx::query("UPDATE eye_api_credentials SET revoked_at_ms=$2 WHERE credential_id=$1")
        .bind(credential_id)
        .bind(now_ms_i64)
        .execute(&mut *tx)
        .await?;

    let created = insert_credential(
        &mut tx,
        tenant_id,
        &client_id,
        actor_user_id,
        &scopes,
        expires_at_ms,
        now_ms_i64,
        Some(credential_id),
        environment,
    )
    .await?;

    sqlx::query(
        "INSERT INTO eye_api_credential_audit (tenant_id, client_id, credential_id, actor_user_id, action, created_at_ms)          VALUES ($1,$2,$3,$4,'CREDENTIAL_ROTATED',$5)",
    )
    .bind(tenant_id.as_str())
    .bind(client_id)
    .bind(credential_id)
    .bind(actor_user_id)
    .bind(now_ms_i64)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(created)
}

/// Atomically charge both tenant-wide and credential-specific minute buckets.
/// The caller supplies limits from trusted server configuration/client policy.
pub async fn consume_rate_limit(
    pool: &PgPool,
    tenant_id: &TenantId,
    credential_id: &str,
    now_ms: u64,
    tenant_limit: i32,
    credential_limit: i32,
) -> Result<bool, ApiCredentialError> {
    if tenant_limit <= 0 || credential_limit <= 0 {
        return Err(ApiCredentialError::InvalidInput);
    }
    let now_ms = to_i64(now_ms)?;
    let window_start_ms = (now_ms / 60_000) * 60_000;
    let mut tx = pool.begin().await?;

    let active: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM eye_api_credentials WHERE tenant_id=$1 AND credential_id=$2 AND revoked_at_ms IS NULL AND (expires_at_ms IS NULL OR expires_at_ms > $3))",
    )
    .bind(tenant_id.as_str())
    .bind(credential_id)
    .bind(now_ms)
    .fetch_one(&mut *tx)
    .await?;
    if !active {
        return Err(ApiCredentialError::InvalidCredential);
    }

    let tenant_count: i32 = sqlx::query_scalar(
        "INSERT INTO eye_api_rate_limit_buckets (tenant_id, dimension, subject_id, credential_id, window_start_ms, request_count) VALUES ($1,'TENANT',$1,NULL,$2,1) ON CONFLICT (tenant_id, dimension, subject_id, window_start_ms) DO UPDATE SET request_count=eye_api_rate_limit_buckets.request_count+1 RETURNING request_count",
    )
    .bind(tenant_id.as_str())
    .bind(window_start_ms)
    .fetch_one(&mut *tx)
    .await?;

    let credential_count: i32 = sqlx::query_scalar(
        "INSERT INTO eye_api_rate_limit_buckets (tenant_id, dimension, subject_id, credential_id, window_start_ms, request_count) VALUES ($1,'CREDENTIAL',$2,$2,$3,1) ON CONFLICT (tenant_id, dimension, subject_id, window_start_ms) DO UPDATE SET request_count=eye_api_rate_limit_buckets.request_count+1 RETURNING request_count",
    )
    .bind(tenant_id.as_str())
    .bind(credential_id)
    .bind(window_start_ms)
    .fetch_one(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(tenant_count <= tenant_limit && credential_count <= credential_limit)
}
