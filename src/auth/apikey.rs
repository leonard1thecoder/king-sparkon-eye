//! Developer and device API-key management, storage, hashing, and verification (issue #46).
//!
//! Implements secure programmatic API key generation, one-way hashed storage, scope enforcement,
//! and instant revocation according to `docs/AUTH_BOUNDARY_CONTRACTS.md` (issue #42).
//!
//! Non-negotiable rules enforced here:
//! - Raw API keys are structured (`kse_<env>_<random>`) and displayed **only once** upon creation.
//! - The raw key secret is never stored on disk, never placed in logs, and never returned in lookups.
//! - The database stores only a salted SHA-256 hash (`key_hash`) and an 8-character lookup prefix (`key_id`).
//! - Expired or revoked keys are strictly refused.
//! - Scope verification ensures least-privilege device and operator authorization.

use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use uuid::Uuid;

use crate::error::ValidationError;

/// Prefix identifying King Sparkon Eye API keys.
pub const API_KEY_PREFIX: &str = "kse_";

/// Public lookup key ID prefix length (e.g. `kse_live_8f3a9e2d`).
pub const API_KEY_ID_LEN: usize = 16;

/// Errors arising from API key operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiKeyError {
    #[error("API key is invalid or does not exist")]
    InvalidKey,
    #[error("API key is revoked")]
    Revoked,
    #[error("API key has expired")]
    Expired,
    #[error("insufficient scope for this operation")]
    InsufficientScope,
    #[error("validation error: {0}")]
    Validation(#[from] ValidationError),
    #[error("key store is unavailable")]
    StoreUnavailable,
}

/// Stored record representing an API key. Never contains the raw secret.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyRecord {
    pub key_id: String,
    pub key_hash: String,
    pub salt: String,
    pub owner_id: String,
    pub label: String,
    pub scopes: Vec<String>,
    pub expires_at_ms: Option<u64>,
    pub revoked: bool,
    pub created_at_ms: u64,
}

/// Newly created API key bundle. This is the **only** time the raw key secret is accessible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedApiKey {
    pub raw_key: String,
    pub record: ApiKeyRecord,
}

/// Computes salted SHA-256 hash of an API key secret.
pub fn hash_api_key(raw_key: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(raw_key.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Storage interface for API keys.
pub trait ApiKeyStore: Send + Sync {
    fn save_key(&self, record: ApiKeyRecord) -> Result<(), ApiKeyError>;
    fn get_key_by_id(&self, key_id: &str) -> Result<Option<ApiKeyRecord>, ApiKeyError>;
    fn revoke_key(&self, key_id: &str) -> Result<(), ApiKeyError>;
    fn list_keys_by_owner(&self, owner_id: &str) -> Result<Vec<ApiKeyRecord>, ApiKeyError>;
}

/// In-memory implementation of [`ApiKeyStore`] for testing and CI.
#[derive(Debug, Default)]
pub struct MemoryApiKeyStore {
    keys: Mutex<HashMap<String, ApiKeyRecord>>,
}

impl MemoryApiKeyStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ApiKeyStore for MemoryApiKeyStore {
    fn save_key(&self, record: ApiKeyRecord) -> Result<(), ApiKeyError> {
        let mut map = self
            .keys
            .lock()
            .map_err(|_| ApiKeyError::StoreUnavailable)?;
        map.insert(record.key_id.clone(), record);
        Ok(())
    }

    fn get_key_by_id(&self, key_id: &str) -> Result<Option<ApiKeyRecord>, ApiKeyError> {
        let map = self
            .keys
            .lock()
            .map_err(|_| ApiKeyError::StoreUnavailable)?;
        Ok(map.get(key_id).cloned())
    }

    fn revoke_key(&self, key_id: &str) -> Result<(), ApiKeyError> {
        let mut map = self
            .keys
            .lock()
            .map_err(|_| ApiKeyError::StoreUnavailable)?;
        if let Some(entry) = map.get_mut(key_id) {
            entry.revoked = true;
            Ok(())
        } else {
            Err(ApiKeyError::InvalidKey)
        }
    }

    fn list_keys_by_owner(&self, owner_id: &str) -> Result<Vec<ApiKeyRecord>, ApiKeyError> {
        let map = self
            .keys
            .lock()
            .map_err(|_| ApiKeyError::StoreUnavailable)?;
        Ok(map
            .values()
            .filter(|k| k.owner_id == owner_id)
            .cloned()
            .collect())
    }
}

/// API Key management and verification service.
pub struct ApiKeyService<S: ApiKeyStore> {
    store: S,
    environment: String,
}

impl<S: ApiKeyStore> ApiKeyService<S> {
    pub fn new(store: S, environment: &str) -> Self {
        Self {
            store,
            environment: environment.to_string(),
        }
    }

    /// Generates a new API key, stores its hash, and returns the single-exposure raw secret.
    pub fn create_key(
        &self,
        owner_id: &str,
        label: &str,
        scopes: Vec<String>,
        ttl_ms: Option<u64>,
        now_ms: u64,
    ) -> Result<CreatedApiKey, ApiKeyError> {
        if owner_id.is_empty() {
            return Err(ApiKeyError::Validation(ValidationError {
                field: "ownerId",
                rule: "must not be empty",
            }));
        }

        // Generate full random key: kse_<env>_<uuid1><uuid2>
        let random_part = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let raw_key = format!("{}{}_{}", API_KEY_PREFIX, self.environment, random_part);

        // Derive key_id from prefix and first 8 chars of random part for fast indexing
        let key_id = raw_key.chars().take(API_KEY_ID_LEN).collect::<String>();

        let salt = Uuid::new_v4().simple().to_string();
        let key_hash = hash_api_key(&raw_key, &salt);

        let expires_at_ms = ttl_ms.map(|ttl| now_ms + ttl);

        let record = ApiKeyRecord {
            key_id,
            key_hash,
            salt,
            owner_id: owner_id.to_string(),
            label: label.to_string(),
            scopes,
            expires_at_ms,
            revoked: false,
            created_at_ms: now_ms,
        };

        self.store.save_key(record.clone())?;

        Ok(CreatedApiKey { raw_key, record })
    }

    /// Authenticates a submitted raw API key, enforcing non-revocation, expiration, and scope.
    pub fn verify_key(
        &self,
        raw_key: &str,
        required_scope: Option<&str>,
        now_ms: u64,
    ) -> Result<ApiKeyRecord, ApiKeyError> {
        if raw_key.len() < API_KEY_ID_LEN || !raw_key.starts_with(API_KEY_PREFIX) {
            return Err(ApiKeyError::InvalidKey);
        }

        let key_id: String = raw_key.chars().take(API_KEY_ID_LEN).collect();
        let record = match self.store.get_key_by_id(&key_id)? {
            Some(r) => r,
            None => {
                let _ = hash_api_key(raw_key, "dummy_salt_for_timing");
                return Err(ApiKeyError::InvalidKey);
            }
        };

        if record.revoked {
            return Err(ApiKeyError::Revoked);
        }

        if let Some(exp) = record.expires_at_ms
            && now_ms >= exp
        {
            return Err(ApiKeyError::Expired);
        }

        let candidate_hash = hash_api_key(raw_key, &record.salt);
        if candidate_hash != record.key_hash {
            return Err(ApiKeyError::InvalidKey);
        }

        if let Some(scope) = required_scope
            && !record.scopes.iter().any(|s| s == scope || s == "admin:all")
        {
            return Err(ApiKeyError::InsufficientScope);
        }

        Ok(record)
    }

    /// Revokes an API key by its public key ID.
    pub fn revoke_key(&self, key_id: &str) -> Result<(), ApiKeyError> {
        self.store.revoke_key(key_id)
    }
}
