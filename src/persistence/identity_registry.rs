//! Tenant-scoped identity registry persistence (#185).
//!
//! External references are unique only inside a tenant. All reads and writes include
//! the tenant resolved from a validated machine credential; caller-selected tenant IDs
//! are never accepted by this module.

use serde_json::Value;
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::persistence::tenants::TenantId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub identity_id: String,
    pub tenant_id: TenantId,
    pub external_reference: String,
    pub status: String,
    pub enrollment_status: String,
    pub metadata: Value,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum IdentityStoreError {
    #[error("identity input is invalid")]
    InvalidInput,
    #[error("identity was not found")]
    NotFound,
    #[error("identity external reference already exists")]
    Conflict,
    #[error("identity persistence is unavailable")]
    Unavailable,
}

impl From<sqlx::Error> for IdentityStoreError {
    fn from(error: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db) = &error {
            if db.code().as_deref() == Some("23505") {
                return Self::Conflict;
            }
        }
        Self::Unavailable
    }
}

fn validate_external_reference(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

fn validate_metadata(value: &Value) -> bool {
    value.is_object()
        && serde_json::to_vec(value).is_ok_and(|encoded| encoded.len() <= 4096)
}

fn map_row(row: sqlx::postgres::PgRow) -> Result<Identity, IdentityStoreError> {
    let raw_tenant: String = row.try_get("tenant_id").map_err(|_| IdentityStoreError::Unavailable)?;
    Ok(Identity {
        identity_id: row.try_get("identity_id").map_err(|_| IdentityStoreError::Unavailable)?,
        tenant_id: TenantId::parse(raw_tenant).map_err(|_| IdentityStoreError::Unavailable)?,
        external_reference: row.try_get("external_reference").map_err(|_| IdentityStoreError::Unavailable)?,
        status: row.try_get("status").map_err(|_| IdentityStoreError::Unavailable)?,
        enrollment_status: row.try_get("enrollment_status").map_err(|_| IdentityStoreError::Unavailable)?,
        metadata: row.try_get("metadata").map_err(|_| IdentityStoreError::Unavailable)?,
        created_at_ms: row.try_get("created_at_ms").map_err(|_| IdentityStoreError::Unavailable)?,
        updated_at_ms: row.try_get("updated_at_ms").map_err(|_| IdentityStoreError::Unavailable)?,
    })
}

const IDENTITY_COLUMNS: &str = "identity_id, tenant_id, external_reference, status, enrollment_status, metadata, created_at_ms, updated_at_ms";

pub async fn create_identity(
    pool: &PgPool,
    tenant_id: &TenantId,
    external_reference: &str,
    metadata: Value,
    client_id: &str,
    actor_user_id: &str,
    now_ms: i64,
) -> Result<Identity, IdentityStoreError> {
    if !validate_external_reference(external_reference)
        || !validate_metadata(&metadata)
        || now_ms <= 0
    {
        return Err(IdentityStoreError::InvalidInput);
    }
    let identity_id = format!("idy_{}", Uuid::new_v4().simple());
    let query = format!(
        "INSERT INTO eye_identities (identity_id, tenant_id, external_reference, metadata, created_at_ms, updated_at_ms) \
         SELECT $1, t.tenant_id, $3, $4, $5, $5 FROM eye_tenants t \
         JOIN eye_api_clients c ON c.tenant_id=t.tenant_id AND c.client_id=$6 AND c.owner_user_id=$7 AND c.status='ACTIVE' \
         JOIN eye_auth_users u ON u.user_id=c.owner_user_id AND u.is_active=TRUE \
         WHERE t.tenant_id=$2 AND t.status='ACTIVE' RETURNING {IDENTITY_COLUMNS}"
    );
    let mut tx = pool.begin().await?;
    let row = sqlx::query(&query)
        .bind(&identity_id)
        .bind(tenant_id.as_str())
        .bind(external_reference)
        .bind(metadata)
        .bind(now_ms)
        .bind(client_id)
        .bind(actor_user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(IdentityStoreError::NotFound)?;
    sqlx::query(
        "INSERT INTO eye_identity_audit (tenant_id, identity_id, client_id, actor_user_id, action, created_at_ms) VALUES ($1,$2,$3,$4,'IDENTITY_CREATED',$5)",
    )
    .bind(tenant_id.as_str())
    .bind(&identity_id)
    .bind(client_id)
    .bind(actor_user_id)
    .bind(now_ms)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    map_row(row)
}


/// A durable enrollment intent. While #95 is blocked, no capture or template is stored
/// and every new intent is persisted as BLOCKED_EVALUATION rather than queued for inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityEnrollment {
    pub enrollment_id: String,
    pub identity_id: String,
    pub status: String,
    pub blocked_reason: Option<String>,
    pub consent_reference: String,
    pub consent_policy_version: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrollmentIntentOutcome {
    Created(IdentityEnrollment),
    Existing(IdentityEnrollment),
}

fn map_enrollment_row(row: sqlx::postgres::PgRow) -> Result<IdentityEnrollment, IdentityStoreError> {
    Ok(IdentityEnrollment {
        enrollment_id: row.try_get("enrollment_id").map_err(|_| IdentityStoreError::Unavailable)?,
        identity_id: row.try_get("identity_id").map_err(|_| IdentityStoreError::Unavailable)?,
        status: row.try_get("status").map_err(|_| IdentityStoreError::Unavailable)?,
        blocked_reason: row.try_get("blocked_reason").map_err(|_| IdentityStoreError::Unavailable)?,
        consent_reference: row.try_get("consent_reference").map_err(|_| IdentityStoreError::Unavailable)?,
        consent_policy_version: row.try_get("consent_policy_version").map_err(|_| IdentityStoreError::Unavailable)?,
        created_at_ms: row.try_get("created_at_ms").map_err(|_| IdentityStoreError::Unavailable)?,
        updated_at_ms: row.try_get("updated_at_ms").map_err(|_| IdentityStoreError::Unavailable)?,
    })
}

const ENROLLMENT_COLUMNS: &str = "enrollment_id, identity_id, status, blocked_reason, consent_reference, consent_policy_version, created_at_ms, updated_at_ms";

fn valid_consent_reference(value: &str) -> bool {
    (8..=128).contains(&value.len())
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_policy_version(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

fn valid_idempotency_key(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Persists an enrollment intent without accepting biometric bytes. Until #95 is
/// explicitly resolved, the gate is closed in code and the intent is BLOCKED_EVALUATION.
pub async fn create_enrollment_intent(
    pool: &PgPool,
    tenant_id: &TenantId,
    identity_id: &str,
    client_id: &str,
    actor_user_id: &str,
    consent_reference: &str,
    consent_policy_version: &str,
    idempotency_key: &str,
    now_ms: i64,
) -> Result<EnrollmentIntentOutcome, IdentityStoreError> {
    if !valid_consent_reference(consent_reference)
        || !valid_policy_version(consent_policy_version)
        || !valid_idempotency_key(idempotency_key)
        || now_ms <= 0
    {
        return Err(IdentityStoreError::InvalidInput);
    }

    let mut tx = pool.begin().await?;
    let lock_key = format!("eye-enrollment:{}:{}:{}", tenant_id.as_str(), identity_id, idempotency_key);
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(lock_key)
        .execute(&mut *tx)
        .await?;

    let existing = sqlx::query(&format!(
        "SELECT {ENROLLMENT_COLUMNS} FROM eye_identity_enrollments WHERE tenant_id=$1 AND identity_id=$2 AND idempotency_key=$3 FOR UPDATE"
    ))
    .bind(tenant_id.as_str())
    .bind(identity_id)
    .bind(idempotency_key)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(row) = existing {
        let enrollment = map_enrollment_row(row)?;
        if enrollment.consent_reference != consent_reference
            || enrollment.consent_policy_version != consent_policy_version
        {
            return Err(IdentityStoreError::Conflict);
        }
        tx.commit().await?;
        return Ok(EnrollmentIntentOutcome::Existing(enrollment));
    }

    // This gate is intentionally not configurable by request headers or tenant metadata.
    // #95 requires consented real-face evaluation data and approved operating thresholds.
    const EVALUATION_GATE_OPEN: bool = false;
    let (status, blocked_reason) = if EVALUATION_GATE_OPEN {
        ("QUEUED", None::<&str>)
    } else {
        ("BLOCKED_EVALUATION", Some("EVALUATION_DATA_REQUIRED"))
    };

    let enrollment_id = format!("enr_{}", Uuid::new_v4().simple());
    let row = sqlx::query(&format!(
        "INSERT INTO eye_identity_enrollments (enrollment_id, tenant_id, identity_id, client_id, actor_user_id, consent_reference, consent_policy_version, idempotency_key, status, blocked_reason, created_at_ms, updated_at_ms)          SELECT $1, i.tenant_id, i.identity_id, $4, $5, $6, $7, $8, $9, $10, $11, $11          FROM eye_identities i JOIN eye_tenants t ON t.tenant_id=i.tenant_id          JOIN eye_api_clients c ON c.tenant_id=i.tenant_id AND c.client_id=$4 AND c.owner_user_id=$5 AND c.status='ACTIVE'          JOIN eye_auth_users u ON u.user_id=c.owner_user_id AND u.is_active=TRUE          WHERE i.tenant_id=$2 AND i.identity_id=$3 AND i.status='ACTIVE' AND t.status='ACTIVE'          RETURNING {ENROLLMENT_COLUMNS}"
    ))
    .bind(&enrollment_id)
    .bind(tenant_id.as_str())
    .bind(identity_id)
    .bind(client_id)
    .bind(actor_user_id)
    .bind(consent_reference)
    .bind(consent_policy_version)
    .bind(idempotency_key)
    .bind(status)
    .bind(blocked_reason)
    .bind(now_ms)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(IdentityStoreError::NotFound)?;

    sqlx::query(
        "UPDATE eye_identities SET enrollment_status=$3, updated_at_ms=$4 WHERE tenant_id=$1 AND identity_id=$2 AND status='ACTIVE'"
    )
    .bind(tenant_id.as_str())
    .bind(identity_id)
    .bind(status)
    .bind(now_ms)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        "INSERT INTO eye_identity_audit (tenant_id, identity_id, client_id, actor_user_id, action, created_at_ms, metadata) VALUES ($1,$2,$3,$4,'ENROLLMENT_BLOCKED',$5,$6)"
    )
    .bind(tenant_id.as_str())
    .bind(identity_id)
    .bind(client_id)
    .bind(actor_user_id)
    .bind(now_ms)
    .bind(serde_json::json!({"enrollmentId": enrollment_id, "reason": blocked_reason}))
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok(EnrollmentIntentOutcome::Created(map_enrollment_row(row)?))
}

pub async fn get_enrollment_intent(
    pool: &PgPool,
    tenant_id: &TenantId,
    identity_id: &str,
    enrollment_id: &str,
) -> Result<IdentityEnrollment, IdentityStoreError> {
    let row = sqlx::query(&format!(
        "SELECT {ENROLLMENT_COLUMNS} FROM eye_identity_enrollments WHERE tenant_id=$1 AND identity_id=$2 AND enrollment_id=$3"
    ))
    .bind(tenant_id.as_str())
    .bind(identity_id)
    .bind(enrollment_id)
    .fetch_optional(pool)
    .await?
    .ok_or(IdentityStoreError::NotFound)?;
    map_enrollment_row(row)
}

pub async fn get_identity(
    pool: &PgPool,
    tenant_id: &TenantId,
    identity_id: &str,
) -> Result<Identity, IdentityStoreError> {
    let query = format!(
        "SELECT {IDENTITY_COLUMNS} FROM eye_identities WHERE tenant_id=$1 AND identity_id=$2 AND status <> 'DELETED'"
    );
    let row = sqlx::query(&query)
        .bind(tenant_id.as_str())
        .bind(identity_id)
        .fetch_optional(pool)
        .await?
        .ok_or(IdentityStoreError::NotFound)?;
    map_row(row)
}

pub async fn revoke_identity(
    pool: &PgPool,
    tenant_id: &TenantId,
    identity_id: &str,
    client_id: &str,
    actor_user_id: &str,
    now_ms: i64,
) -> Result<Identity, IdentityStoreError> {
    if now_ms <= 0 {
        return Err(IdentityStoreError::InvalidInput);
    }
    let mut tx = pool.begin().await?;
    let query = format!(
        "UPDATE eye_identities SET status='REVOKED', enrollment_status='REVOKED', revoked_at_ms=COALESCE(revoked_at_ms,$5), updated_at_ms=$5 \
         WHERE tenant_id=$1 AND identity_id=$2 AND status='ACTIVE' AND EXISTS ( \
           SELECT 1 FROM eye_api_clients c JOIN eye_tenants t ON t.tenant_id=c.tenant_id \
           JOIN eye_auth_users u ON u.user_id=c.owner_user_id \
           WHERE c.tenant_id=$1 AND c.client_id=$3 AND c.owner_user_id=$4 \
             AND c.status='ACTIVE' AND t.status='ACTIVE' AND u.is_active=TRUE \
         ) RETURNING {IDENTITY_COLUMNS}"
    );
    let row = sqlx::query(&query)
        .bind(tenant_id.as_str())
        .bind(identity_id)
        .bind(client_id)
        .bind(actor_user_id)
        .bind(now_ms)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(IdentityStoreError::NotFound)?;
    sqlx::query(
        "INSERT INTO eye_identity_audit (tenant_id, identity_id, client_id, actor_user_id, action, created_at_ms) VALUES ($1,$2,$3,$4,'IDENTITY_REVOKED',$5)",
    )
    .bind(tenant_id.as_str())
    .bind(identity_id)
    .bind(client_id)
    .bind(actor_user_id)
    .bind(now_ms)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    map_row(row)
}

pub async fn delete_identity(
    pool: &PgPool,
    tenant_id: &TenantId,
    identity_id: &str,
    client_id: &str,
    actor_user_id: &str,
    now_ms: i64,
) -> Result<(), IdentityStoreError> {
    if now_ms <= 0 {
        return Err(IdentityStoreError::InvalidInput);
    }
    let mut tx = pool.begin().await?;
    let updated = sqlx::query(
        "UPDATE eye_identities SET status='DELETED', enrollment_status='REVOKED', deleted_at_ms=COALESCE(deleted_at_ms,$5), updated_at_ms=$5 \
         WHERE tenant_id=$1 AND identity_id=$2 AND status <> 'DELETED' AND EXISTS ( \
           SELECT 1 FROM eye_api_clients c JOIN eye_tenants t ON t.tenant_id=c.tenant_id \
           JOIN eye_auth_users u ON u.user_id=c.owner_user_id \
           WHERE c.tenant_id=$1 AND c.client_id=$3 AND c.owner_user_id=$4 \
             AND c.status='ACTIVE' AND t.status='ACTIVE' AND u.is_active=TRUE \
         )",
    )
    .bind(tenant_id.as_str())
    .bind(identity_id)
    .bind(client_id)
    .bind(actor_user_id)
    .bind(now_ms)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(IdentityStoreError::NotFound);
    }
    sqlx::query(
        "INSERT INTO eye_identity_audit (tenant_id, identity_id, client_id, actor_user_id, action, created_at_ms) VALUES ($1,$2,$3,$4,'IDENTITY_DELETED',$5)",
    )
    .bind(tenant_id.as_str())
    .bind(identity_id)
    .bind(client_id)
    .bind(actor_user_id)
    .bind(now_ms)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_reference_validation_is_bounded_and_control_free() {
        assert!(validate_external_reference("app-user-42"));
        assert!(!validate_external_reference(""));
        assert!(!validate_external_reference(" padded "));
        assert!(!validate_external_reference(&"x".repeat(129)));
        assert!(!validate_external_reference("user\n42"));
    }

    #[test]
    fn metadata_must_be_a_small_json_object() {
        assert!(validate_metadata(&serde_json::json!({"source":"crm"})));
        assert!(!validate_metadata(&serde_json::json!(["not", "an", "object"])));
        assert!(!validate_metadata(&serde_json::json!({"payload":"x".repeat(5000)})));
    }
}
