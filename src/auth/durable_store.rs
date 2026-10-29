//! Durable Eye identity, OAuth transaction and refresh-token storage.
//!
//! Operations are exposed as versioned PostgreSQL RPC functions through the service-role
//! PostgREST endpoint. The database migration lives in db/migrations. No provider tokens,
//! raw handoff codes or refresh tokens are persisted.

use std::time::Duration;

use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::auth::{UserRole, UserRecord, hash_password};
use crate::config::OAuthRuntimeConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DurableStoreError {
    #[error("durable identity store is unavailable")]
    Unavailable,
    #[error("durable identity store rejected the operation")]
    Rejected,
    #[error("durable identity record is invalid")]
    InvalidRecord,
}

#[derive(Clone)]
pub struct DurableEyeStore {
    client: Client,
    base_url: String,
    service_key: String,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DurableUser {
    pub user_id: String,
    pub email: String,
    pub role: UserRole,
    pub scopes: Vec<String>,
    pub is_active: bool,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OAuthTransaction {
    pub nonce: String,
    pub code_verifier: String,
    pub callback_url: String,
    pub state_valid: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ConsumedHandoff {
    pub user_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BooleanResult {
    pub ok: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RateLimitResult {
    pub allowed: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RefreshRotation {
    pub outcome: String,
    pub user_id: Option<String>,
    pub email: Option<String>,
    pub role: Option<UserRole>,
    pub scopes: Option<Vec<String>>,
    pub is_active: Option<bool>,
    pub created_at_ms: Option<u64>,
    pub event_id: Option<String>,
    pub family_id: Option<String>,
}

impl DurableEyeStore {
    pub fn new(config: &OAuthRuntimeConfig) -> Self {
        Self {
            client: Client::new(),
            base_url: config.database_rest_url.trim_end_matches('/').to_owned(),
            service_key: config.database_service_role_key.expose().to_owned(),
        }
    }

    async fn rpc<T: DeserializeOwned>(
        &self,
        function: &str,
        body: Value,
    ) -> Result<Vec<T>, DurableStoreError> {
        let response = self.client
            .post(format!("{}/rpc/{function}", self.base_url))
            .header("apikey", &self.service_key)
            .bearer_auth(&self.service_key)
            .timeout(Duration::from_secs(5))
            .json(&body)
            .send()
            .await
            .map_err(|_| DurableStoreError::Unavailable)?;
        if response.status() == StatusCode::UNAUTHORIZED || response.status() == StatusCode::FORBIDDEN {
            return Err(DurableStoreError::Rejected);
        }
        if !response.status().is_success() {
            return Err(DurableStoreError::Unavailable);
        }
        let bytes = response.bytes().await.map_err(|_| DurableStoreError::Unavailable)?;
        if bytes.len() > 64 * 1024 {
            return Err(DurableStoreError::InvalidRecord);
        }
        serde_json::from_slice(&bytes).map_err(|_| DurableStoreError::InvalidRecord)
    }

    pub async fn create_oauth_transaction(
        &self, cookie_hash: &str, state_hash: &str, nonce: &str,
        code_verifier: &str, callback_url: &str, now_ms: u64,
    ) -> Result<(), DurableStoreError> {
        let rows: Vec<BooleanResult> = self.rpc("eye_create_oauth_transaction", json!({
            "p_cookie_hash": cookie_hash, "p_state_hash": state_hash, "p_nonce": nonce,
            "p_code_verifier": code_verifier, "p_callback_url": callback_url,
            "p_now_ms": now_ms, "p_ttl_seconds": 600
        })).await?;
        if rows.first().is_some_and(|row| row.ok) { Ok(()) } else { Err(DurableStoreError::Unavailable) }
    }

    /// Consumes the record even when the state is wrong, so failed callbacks cannot be retried.
    pub async fn consume_oauth_transaction(
        &self, cookie_hash: &str, state_hash: &str, now_ms: u64,
    ) -> Result<Option<OAuthTransaction>, DurableStoreError> {
        let rows: Vec<OAuthTransaction> = self.rpc("eye_consume_oauth_transaction", json!({
            "p_cookie_hash": cookie_hash, "p_state_hash": state_hash, "p_now_ms": now_ms
        })).await?;
        Ok(rows.into_iter().next())
    }

    pub async fn resolve_google_identity(
        &self, provider_subject: &str, email: &str, email_verified: bool,
        now_ms: u64, new_user: &UserRecord,
    ) -> Result<DurableUser, DurableStoreError> {
        let rows: Vec<DurableUser> = self.rpc("eye_resolve_google_identity", json!({
            "p_provider_subject": provider_subject, "p_email": email,
            "p_email_verified": email_verified, "p_new_user_id": new_user.user_id,
            "p_new_password_hash": new_user.password_hash, "p_new_salt": new_user.salt,
            "p_now_ms": now_ms
        })).await?;
        rows.into_iter().next().ok_or(DurableStoreError::Rejected)
    }

    pub async fn user_by_id(&self, user_id: &str) -> Result<Option<DurableUser>, DurableStoreError> {
        let rows: Vec<DurableUser> = self.rpc("eye_user_by_id", json!({"p_user_id": user_id})).await?;
        Ok(rows.into_iter().next())
    }

    pub async fn issue_handoff(
        &self, code_hash: &str, user_id: &str, callback_url: &str, now_ms: u64,
    ) -> Result<(), DurableStoreError> {
        let rows: Vec<BooleanResult> = self.rpc("eye_issue_oauth_handoff", json!({
            "p_code_hash": code_hash, "p_user_id": user_id, "p_callback_url": callback_url,
            "p_now_ms": now_ms, "p_ttl_seconds": 60
        })).await?;
        if rows.first().is_some_and(|row| row.ok) { Ok(()) } else { Err(DurableStoreError::Unavailable) }
    }

    /// Atomically removes a handoff before validating expiry and callback binding.
    pub async fn consume_handoff(
        &self, code_hash: &str, callback_url: &str, now_ms: u64,
    ) -> Result<Option<ConsumedHandoff>, DurableStoreError> {
        let rows: Vec<ConsumedHandoff> = self.rpc("eye_consume_oauth_handoff", json!({
            "p_code_hash": code_hash, "p_callback_url": callback_url, "p_now_ms": now_ms
        })).await?;
        Ok(rows.into_iter().next())
    }

    pub async fn allow_oauth_attempt(
        &self, bucket_hash: &str, now_ms: u64, window_ms: u64, max_attempts: u32,
    ) -> Result<bool, DurableStoreError> {
        let rows: Vec<RateLimitResult> = self.rpc("eye_allow_oauth_attempt", json!({
            "p_bucket_hash": bucket_hash, "p_now_ms": now_ms,
            "p_window_ms": window_ms, "p_max_attempts": max_attempts
        })).await?;
        Ok(rows.first().is_some_and(|row| row.allowed))
    }

    pub async fn issue_refresh_token(
        &self, token_hash: &str, family_id: &str, user: &DurableUser,
        event_id: Option<&str>, now_ms: u64, expires_at_ms: u64,
    ) -> Result<(), DurableStoreError> {
        let rows: Vec<BooleanResult> = self.rpc("eye_issue_refresh_token", json!({
            "p_token_hash": token_hash, "p_family_id": family_id,
            "p_user_id": user.user_id, "p_role": role_name(user.role), "p_scopes": user.scopes,
            "p_event_id": event_id, "p_now_ms": now_ms, "p_expires_at_ms": expires_at_ms
        })).await?;
        if rows.first().is_some_and(|row| row.ok) { Ok(()) } else { Err(DurableStoreError::Rejected) }
    }

    /// Consumes the old token and inserts the replacement atomically in PostgreSQL.
    pub async fn rotate_refresh_token(
        &self, old_token_hash: &str, new_token_hash: &str, now_ms: u64,
        new_expires_at_ms: u64,
    ) -> Result<RefreshRotation, DurableStoreError> {
        let rows: Vec<RefreshRotation> = self.rpc("eye_rotate_refresh_token", json!({
            "p_old_token_hash": old_token_hash, "p_new_token_hash": new_token_hash,
            "p_now_ms": now_ms, "p_new_expires_at_ms": new_expires_at_ms
        })).await?;
        rows.into_iter().next().ok_or(DurableStoreError::Rejected)
    }
}

pub fn hash_hex(value: &str) -> String {
    Sha256::digest(value.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn role_name(role: UserRole) -> &'static str {
    match role {
        UserRole::GateOperator => "GateOperator",
        UserRole::Admin => "Admin",
        UserRole::Auditor => "Auditor",
    }
}

pub fn new_oauth_user(email: &str, now_ms: u64) -> Result<UserRecord, DurableStoreError> {
    let salt = crate::auth::google::random_urlsafe(32).map_err(|_| DurableStoreError::Unavailable)?;
    let one_time_password = crate::auth::google::random_urlsafe(32).map_err(|_| DurableStoreError::Unavailable)?;
    Ok(UserRecord {
        user_id: format!("usr_{}", crate::auth::google::random_urlsafe(16).map_err(|_| DurableStoreError::Unavailable)?),
        email: email.to_owned(),
        password_hash: hash_password(&one_time_password, &salt),
        salt,
        role: UserRole::GateOperator,
        scopes: vec!["enrollment:write".to_owned()],
        is_active: true,
        created_at_ms: now_ms,
    })
}
