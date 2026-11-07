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
