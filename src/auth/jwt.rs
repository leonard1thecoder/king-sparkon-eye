//! Signed JWT access tokens, refresh rotation, and session management (issue #44).
//!
//! Implements signed token generation, signature and claims verification, and refresh token
//! family rotation according to the contracts defined in `docs/AUTH_BOUNDARY_CONTRACTS.md` (issue #42).
//!
//! Rules enforced here:
//! - Access tokens are short-lived (default: 15 minutes / 900 seconds).
//! - Refresh tokens are single-use within an active session family.
//! - Replay of an already-used refresh token revokes the entire session family.
//! - Secret keys, signatures, and raw refresh tokens are never logged.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::hmac;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Mutex;
use uuid::Uuid;

use crate::auth::UserRole;

/// Standard issuer for Eye access tokens.
pub const EYE_TOKEN_ISSUER: &str = "https://eye.kingsparkon.internal";

/// Standard audience for Eye access tokens.
pub const EYE_TOKEN_AUDIENCE: &str = "king-sparkon-eye";

/// Maximum clock skew tolerated for `iat` in the future (seconds).
pub const MAX_IAT_SKEW_SECS: u64 = 60;

/// Default access token validity duration in seconds (15 minutes).
pub const ACCESS_TOKEN_TTL_SECS: u64 = 900;

/// Default refresh token validity duration in seconds (7 days).
pub const REFRESH_TOKEN_TTL_SECS: u64 = 7 * 86400;

/// Standard JWT header.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JwtHeader {
    pub alg: String,
    pub typ: String,
    pub kid: String,
}

impl Default for JwtHeader {
    fn default() -> Self {
        Self {
            alg: "HS256".to_string(),
            typ: "JWT".to_string(),
            kid: "key-2026-v1".to_string(),
        }
    }
}

/// Standard claims payload for an Eye access token.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccessTokenClaims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub exp: u64,
    pub iat: u64,
    pub jti: String,
    pub role: UserRole,
    pub scopes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}

/// Errors occurring during token operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("token is expired")]
    Expired,
    #[error("invalid token signature or format")]
    InvalidSignature,
    #[error("invalid token claims: issuer or audience mismatch")]
    InvalidClaims,
    #[error("refresh token has already been used: session revoked")]
    ReplayDetected,
    #[error("refresh token not found or revoked")]
    RefreshTokenNotFound,
    #[error("unsupported token signing algorithm")]
    UnsupportedAlgorithm,
    #[error("token is not yet valid")]
    NotYetValid,
    #[error("serialization error: {0}")]
    Serialization(String),
}

/// Computes HMAC-SHA256 signature representation.
/// HMAC-SHA256 over `header.claims` (RFC 2104), base64url without padding (issue #100).
fn compute_signature(header_b64: &str, claims_b64: &str, secret: &[u8]) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let tag = hmac::sign(&key, format!("{header_b64}.{claims_b64}").as_bytes());
    URL_SAFE_NO_PAD.encode(tag.as_ref())
}

/// Constant-time verification of a base64url HMAC-SHA256 signature over `header.claims`.
fn verify_signature(
    header_b64: &str,
    claims_b64: &str,
    signature_b64: &str,
    secret: &[u8],
) -> bool {
    let Ok(sig_bytes) = URL_SAFE_NO_PAD.decode(signature_b64) else {
        return false;
    };
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    hmac::verify(
        &key,
        format!("{header_b64}.{claims_b64}").as_bytes(),
        &sig_bytes,
    )
    .is_ok()
}

/// Signs an access token into compact JWT string (`header.claims.signature`).
pub fn sign_access_token(
    claims: &AccessTokenClaims,
    kid: &str,
    secret: &[u8],
) -> Result<String, TokenError> {
    let header = JwtHeader {
        alg: "HS256".to_string(),
        typ: "JWT".to_string(),
        kid: kid.to_string(),
    };

    let header_json =
        serde_json::to_vec(&header).map_err(|e| TokenError::Serialization(e.to_string()))?;
    let claims_json =
        serde_json::to_vec(claims).map_err(|e| TokenError::Serialization(e.to_string()))?;

    let header_b64 = URL_SAFE_NO_PAD.encode(&header_json);
    let claims_b64 = URL_SAFE_NO_PAD.encode(&claims_json);
    let signature = compute_signature(&header_b64, &claims_b64, secret);

    Ok(format!("{header_b64}.{claims_b64}.{signature}"))
}

/// Verifies compact JWT string and extracts claims.
pub fn verify_access_token(
    token_str: &str,
    expected_kid: &str,
    secret: &[u8],
    now_secs: u64,
) -> Result<AccessTokenClaims, TokenError> {
    let parts: Vec<&str> = token_str.split('.').collect();
    if parts.len() != 3 {
        return Err(TokenError::InvalidSignature);
    }

    let header_b64 = parts[0];
    let claims_b64 = parts[1];
    let signature = parts[2];

    // Check the header before trusting the signature. Only HS256 is accepted (issue #101).
    let header_bytes = URL_SAFE_NO_PAD
        .decode(header_b64)
        .map_err(|_| TokenError::InvalidSignature)?;
    let header: JwtHeader =
        serde_json::from_slice(&header_bytes).map_err(|_| TokenError::InvalidSignature)?;
    if header.alg != "HS256" {
        return Err(TokenError::UnsupportedAlgorithm);
    }

    if !verify_signature(header_b64, claims_b64, signature, secret) {
        return Err(TokenError::InvalidSignature);
    }

    if header.kid != expected_kid {
        return Err(TokenError::InvalidSignature);
    }

    let claims_bytes = URL_SAFE_NO_PAD
        .decode(claims_b64)
        .map_err(|_| TokenError::InvalidSignature)?;
    let claims: AccessTokenClaims =
        serde_json::from_slice(&claims_bytes).map_err(|_| TokenError::InvalidSignature)?;

    if claims.iss != EYE_TOKEN_ISSUER || claims.aud != EYE_TOKEN_AUDIENCE {
        return Err(TokenError::InvalidClaims);
    }

    // RFC 7519 §4.1.4: the token is not accepted on or after exp.
    if now_secs >= claims.exp {
        return Err(TokenError::Expired);
    }

    // Issued-at must not be meaningfully in the future (issue #101).
    if claims.iat > now_secs + MAX_IAT_SKEW_SECS {
        return Err(TokenError::NotYetValid);
    }

    Ok(claims)
}

/// Internal record of a refresh token.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RefreshEntry {
    token_hash: String,
    family_id: String,
    user_id: String,
    role: UserRole,
    scopes: Vec<String>,
    event_id: Option<String>,
    expires_at_secs: u64,
    used: bool,
}

/// Token response returned on authentication or refresh rotation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenPair {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in_secs: u64,
}

/// Manages access tokens and single-use refresh token families.
pub struct TokenManager {
    secret: Vec<u8>,
    kid: String,
    tokens: Mutex<HashMap<String, RefreshEntry>>, // Map token_hash -> RefreshEntry
    family_revoked: Mutex<HashMap<String, bool>>, // Map family_id -> is_revoked
}

impl TokenManager {
    pub fn new(secret: Vec<u8>, kid: &str) -> Self {
        Self {
            secret,
            kid: kid.to_string(),
            tokens: Mutex::new(HashMap::new()),
            family_revoked: Mutex::new(HashMap::new()),
        }
    }

    fn hash_token(raw: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(raw.as_bytes());
        URL_SAFE_NO_PAD.encode(hasher.finalize())
    }

    /// Issues an initial token pair starting a new session family.
    pub fn issue_token_pair(
        &self,
        user_id: &str,
        role: UserRole,
        scopes: Vec<String>,
        event_id: Option<String>,
        now_secs: u64,
    ) -> Result<TokenPair, TokenError> {
        let family_id = format!("fam_{}", Uuid::new_v4().simple());
        self.issue_tokens_for_family(user_id, role, scopes, event_id, family_id, now_secs)
    }

    fn issue_tokens_for_family(
        &self,
        user_id: &str,
        role: UserRole,
        scopes: Vec<String>,
        event_id: Option<String>,
        family_id: String,
        now_secs: u64,
    ) -> Result<TokenPair, TokenError> {
        let claims = AccessTokenClaims {
            iss: EYE_TOKEN_ISSUER.to_string(),
            sub: user_id.to_string(),
            aud: EYE_TOKEN_AUDIENCE.to_string(),
            exp: now_secs + ACCESS_TOKEN_TTL_SECS,
            iat: now_secs,
            jti: format!("tok_{}", Uuid::new_v4().simple()),
            role,
            scopes: scopes.clone(),
            event_id: event_id.clone(),
        };

        let access_token = sign_access_token(&claims, &self.kid, &self.secret)?;

        let raw_refresh_token = format!("refr_{}", Uuid::new_v4().simple());
        let token_hash = Self::hash_token(&raw_refresh_token);

        let entry = RefreshEntry {
            token_hash: token_hash.clone(),
            family_id,
            user_id: user_id.to_string(),
            role,
            scopes,
            event_id,
            expires_at_secs: now_secs + REFRESH_TOKEN_TTL_SECS,
            used: false,
        };

        if let Ok(mut tokens) = self.tokens.lock() {
            tokens.insert(token_hash, entry);
        }

        Ok(TokenPair {
            access_token,
            refresh_token: raw_refresh_token,
            token_type: "Bearer".to_string(),
            expires_in_secs: ACCESS_TOKEN_TTL_SECS,
        })
    }

    /// Rotates a refresh token: issues a new pair and marks the old one used.
    /// If an already-used token is re-submitted, revokes the entire family.
    pub fn rotate_refresh_token(
        &self,
        raw_refresh_token: &str,
        now_secs: u64,
    ) -> Result<TokenPair, TokenError> {
        let token_hash = Self::hash_token(raw_refresh_token);
        let mut tokens = self
            .tokens
            .lock()
            .map_err(|_| TokenError::RefreshTokenNotFound)?;

        let entry = tokens
            .get_mut(&token_hash)
            .ok_or(TokenError::RefreshTokenNotFound)?;

        // Check if session family was already revoked
        if let Ok(family_revoked) = self.family_revoked.lock()
            && family_revoked
                .get(&entry.family_id)
                .copied()
                .unwrap_or(false)
        {
            return Err(TokenError::ReplayDetected);
        }

        // Replay detection: token was already used! Revoke entire family.
        if entry.used {
            if let Ok(mut family_revoked) = self.family_revoked.lock() {
                family_revoked.insert(entry.family_id.clone(), true);
            }
            return Err(TokenError::ReplayDetected);
        }

        if entry.expires_at_secs < now_secs {
            return Err(TokenError::Expired);
        }

        // Mark as used
        entry.used = true;

        let user_id = entry.user_id.clone();
        let role = entry.role;
        let scopes = entry.scopes.clone();
        let event_id = entry.event_id.clone();
        let family_id = entry.family_id.clone();

        drop(tokens); // release lock before issuing new tokens

        self.issue_tokens_for_family(&user_id, role, scopes, event_id, family_id, now_secs)
    }

    /// Verifies an access token.
    pub fn verify_access(
        &self,
        token_str: &str,
        now_secs: u64,
    ) -> Result<AccessTokenClaims, TokenError> {
        verify_access_token(token_str, &self.kid, &self.secret, now_secs)
    }
}
