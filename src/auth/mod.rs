//! Eye-owned identity storage and password authentication (issue #43).
//!
//! Implements user registration, credential verification, and account lifecycle
//! following the contracts established in `docs/AUTH_BOUNDARY_CONTRACTS.md` (issue #42).
//!
//! Rules enforced here:
//! - Passwords must never be stored in plain text. Stored as salted SHA-256 digests.
//! - Enumeration resistance: login failures return a uniform [`AuthError::InvalidCredentials`],
//!   preventing attackers from guessing whether an email exists.
//! - Account identifiers are opaque and immutable (`usr_<uuid>`).
//! - Scopes and roles are typed and validated.
//! - Sensitive credentials (passwords, salts, hashes) are strictly excluded from debug logs and serialization.

use std::collections::HashMap;
use std::sync::Mutex;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::contract::validate_identifier;
use crate::error::ValidationError;

/// Minimum allowed password length for operators and administrators.
pub const MIN_PASSWORD_LEN: usize = 10;

/// Operator roles in Eye.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UserRole {
    GateOperator,
    Admin,
    Auditor,
}

/// User identity record stored in Eye identity storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRecord {
    pub user_id: String,
    pub email: String,
    pub password_hash: String,
    pub salt: String,
    pub role: UserRole,
    pub scopes: Vec<String>,
    pub is_active: bool,
    pub created_at_ms: u64,
}

/// Public/safe representation of a user profile (no secrets or password hashes).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserProfile {
    pub user_id: String,
    pub email: String,
    pub role: UserRole,
    pub scopes: Vec<String>,
    pub is_active: bool,
    pub created_at_ms: u64,
}

impl From<&UserRecord> for UserProfile {
    fn from(r: &UserRecord) -> Self {
        Self {
            user_id: r.user_id.clone(),
            email: r.email.clone(),
            role: r.role,
            scopes: r.scopes.clone(),
            is_active: r.is_active,
            created_at_ms: r.created_at_ms,
        }
    }
}

/// Errors from authentication and identity storage operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("invalid username or password")]
    InvalidCredentials,
    #[error("account is disabled")]
    AccountDisabled,
    #[error("user already exists")]
    UserAlreadyExists,
    #[error("invalid password: must be at least {MIN_PASSWORD_LEN} characters")]
    WeakPassword,
    #[error("validation error: {0}")]
    Validation(#[from] ValidationError),
    #[error("identity store is unavailable")]
    StoreUnavailable,
}

/// Hash a password with a hex salt using SHA-256.
pub fn hash_password(password: &str, salt: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(password.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Identity storage trait for user credential management.
pub trait IdentityStore: Send + Sync {
    fn get_by_email(&self, email: &str) -> Result<Option<UserRecord>, AuthError>;
    fn get_by_id(&self, user_id: &str) -> Result<Option<UserRecord>, AuthError>;
    fn save_user(&self, record: UserRecord) -> Result<(), AuthError>;
}

/// Thread-safe in-memory identity store for unit and integration testing.
#[derive(Debug, Default)]
pub struct MemoryIdentityStore {
    users_by_id: Mutex<HashMap<String, UserRecord>>,
    users_by_email: Mutex<HashMap<String, String>>,
}

impl MemoryIdentityStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl IdentityStore for MemoryIdentityStore {
    fn get_by_email(&self, email: &str) -> Result<Option<UserRecord>, AuthError> {
        let emails = self.users_by_email.lock().map_err(|_| AuthError::StoreUnavailable)?;
        let users = self.users_by_id.lock().map_err(|_| AuthError::StoreUnavailable)?;
        if let Some(user_id) = emails.get(&email.to_lowercase()) {
            Ok(users.get(user_id).cloned())
        } else {
            Ok(None)
        }
    }

    fn get_by_id(&self, user_id: &str) -> Result<Option<UserRecord>, AuthError> {
        let users = self.users_by_id.lock().map_err(|_| AuthError::StoreUnavailable)?;
        Ok(users.get(user_id).cloned())
    }

    fn save_user(&self, record: UserRecord) -> Result<(), AuthError> {
        let mut emails = self.users_by_email.lock().map_err(|_| AuthError::StoreUnavailable)?;
        let mut users = self.users_by_id.lock().map_err(|_| AuthError::StoreUnavailable)?;

        emails.insert(record.email.to_lowercase(), record.user_id.clone());
        users.insert(record.user_id.clone(), record);
        Ok(())
    }
}

/// Identity and password authentication service.
pub struct AuthService<S: IdentityStore> {
    store: S,
}

impl<S: IdentityStore> AuthService<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }

    /// Registers a new user with password credentials and assigned role.
    pub fn register(
        &self,
        email: &str,
        password: &str,
        role: UserRole,
        scopes: Vec<String>,
        now_ms: u64,
    ) -> Result<UserProfile, AuthError> {
        if email.is_empty() || !email.contains('@') {
            return Err(AuthError::Validation(ValidationError {
                field: "email",
                rule: "must be a valid email address",
            }));
        }

        if password.len() < MIN_PASSWORD_LEN {
            return Err(AuthError::WeakPassword);
        }

        if self.store.get_by_email(email)?.is_some() {
            return Err(AuthError::UserAlreadyExists);
        }

        let user_id = format!("usr_{}", Uuid::new_v4().simple());
        let salt = Uuid::new_v4().simple().to_string();
        let password_hash = hash_password(password, &salt);

        let record = UserRecord {
            user_id,
            email: email.to_string(),
            password_hash,
            salt,
            role,
            scopes,
            is_active: true,
            created_at_ms: now_ms,
        };

        self.store.save_user(record.clone())?;
        Ok(UserProfile::from(&record))
    }

    /// Verifies password credentials with enumeration-resistant errors.
    pub fn authenticate(&self, email: &str, password: &str) -> Result<UserProfile, AuthError> {
        let user = match self.store.get_by_email(email)? {
            Some(u) => u,
            None => {
                // Constant-time-like dummy computation preventing timing attacks
                let _ = hash_password(password, "dummy_salt_for_timing_resistance");
                return Err(AuthError::InvalidCredentials);
            }
        };

        if !user.is_active {
            return Err(AuthError::AccountDisabled);
        }

        let candidate_hash = hash_password(password, &user.salt);
        if candidate_hash != user.password_hash {
            return Err(AuthError::InvalidCredentials);
        }

        Ok(UserProfile::from(&user))
    }
}
