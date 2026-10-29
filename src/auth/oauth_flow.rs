//! Safe, reusable primitives for the approved Eye OAuth handoff contract.
//!
//! These primitives do not mount HTTP routes, perform Google discovery/token exchanges,
//! persist provider links, or provide cross-process durability. They must not be treated as
//! a complete production OAuth flow until those pieces are wired and tested.

use std::collections::HashMap;
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::auth::google::{GoogleAuthError, random_urlsafe};
use crate::contract::validate_identifier;

/// Approved one-time handoff lifetime from the #93 decision record.
pub const OAUTH_HANDOFF_TTL_SECS: u64 = 60;

/// Failures intentionally contain no callback URL, user ID, code or secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum OAuthFlowError {
    #[error("OAuth callback allowlist configuration is invalid")]
    InvalidAllowlist,
    #[error("OAuth callback is not allowlisted")]
    CallbackNotAllowed,
    #[error("OAuth callback URL is invalid for the configured environment")]
    InvalidCallback,
    #[error("OAuth handoff code is invalid, expired or already consumed")]
    InvalidHandoff,
    #[error("OAuth handoff code has expired")]
    HandoffExpired,
    #[error("OAuth handoff callback does not match")]
    CallbackMismatch,
    #[error("OAuth user identifier is invalid")]
    InvalidUser,
    #[error("OAuth random source is unavailable")]
    RandomUnavailable,
    #[error("OAuth handoff store is unavailable")]
    StoreUnavailable,
}

impl From<GoogleAuthError> for OAuthFlowError {
    fn from(_: GoogleAuthError) -> Self {
        Self::RandomUnavailable
    }
}

/// Exact callback allowlist. The localhost HTTP exception must only be enabled in local dev.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallbackAllowlist {
    callbacks: Vec<String>,
    allow_localhost_http: bool,
}

impl CallbackAllowlist {
    pub fn parse(csv: &str, allow_localhost_http: bool) -> Result<Self, OAuthFlowError> {
        let mut callbacks = Vec::new();
        for item in csv.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            validate_callback_url(item, allow_localhost_http)?;
            if !callbacks.iter().any(|existing| existing == item) {
                callbacks.push(item.to_string());
            }
        }
        if callbacks.is_empty() {
            return Err(OAuthFlowError::InvalidAllowlist);
        }
        Ok(Self {
            callbacks,
            allow_localhost_http,
        })
    }

    /// Returns the unchanged URL only when it exactly matches an approved entry.
    pub fn require(&self, callback: &str) -> Result<String, OAuthFlowError> {
        validate_callback_url(callback, self.allow_localhost_http)?;
        if self.callbacks.iter().any(|allowed| allowed == callback) {
            Ok(callback.to_string())
        } else {
            Err(OAuthFlowError::CallbackNotAllowed)
        }
    }

    pub fn entries(&self) -> &[String] {
        &self.callbacks
    }
}

fn validate_callback_url(raw: &str, allow_localhost_http: bool) -> Result<(), OAuthFlowError> {
    if raw.contains('*') || raw.trim() != raw {
        return Err(OAuthFlowError::InvalidCallback);
    }
    let url = reqwest::Url::parse(raw).map_err(|_| OAuthFlowError::InvalidCallback)?;
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(OAuthFlowError::InvalidCallback);
    }

    match url.scheme() {
        "https" => {}
        "http" if allow_localhost_http => {
            if !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")) {
                return Err(OAuthFlowError::InvalidCallback);
            }
        }
        _ => return Err(OAuthFlowError::InvalidCallback),
    }

    // Refuse alternate spellings that URL parsing would normalize; allowlist comparison is exact.
    if url.as_str() != raw {
        return Err(OAuthFlowError::InvalidCallback);
    }
    Ok(())
}

#[derive(Clone)]
struct HandoffEntry {
    user_id: String,
    callback_url: String,
    expires_at_secs: u64,
}

/// Single-process implementation of the #93 handoff semantics.
///
/// Only SHA-256 hashes of opaque handoff codes are kept in memory. Consumption removes the
/// entry under one mutex before validating expiry/callback, so replay and callback mismatch
/// cannot leave a usable code behind. A durable shared store is required before production use.
#[derive(Default)]
pub struct OneTimeHandoffStore {
    entries_by_code_hash: Mutex<HashMap<String, HandoffEntry>>,
}

impl OneTimeHandoffStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issues a random 256-bit handoff code bound to a validated Eye user and exact callback.
    pub fn issue(
        &self,
        user_id: &str,
        callback_url: &str,
        allowlist: &CallbackAllowlist,
        now_secs: u64,
    ) -> Result<String, OAuthFlowError> {
        validate_identifier("userId", user_id).map_err(|_| OAuthFlowError::InvalidUser)?;
        let callback_url = allowlist.require(callback_url)?;
        let expires_at_secs = now_secs
            .checked_add(OAUTH_HANDOFF_TTL_SECS)
            .ok_or(OAuthFlowError::InvalidHandoff)?;
        let raw_code = random_urlsafe(32)?;
        let code_hash = hash_code(&raw_code);
        let mut entries = self
            .entries_by_code_hash
            .lock()
            .map_err(|_| OAuthFlowError::StoreUnavailable)?;
        entries.retain(|_, entry| now_secs < entry.expires_at_secs);
        if entries.contains_key(&code_hash) {
            // A collision at 256 bits is extraordinarily unlikely. Fail closed rather than overwrite.
            return Err(OAuthFlowError::InvalidHandoff);
        }
        entries.insert(
            code_hash,
            HandoffEntry {
                user_id: user_id.to_string(),
                callback_url,
                expires_at_secs,
            },
        );
        Ok(raw_code)
    }

    /// Atomically consumes a handoff code and returns its bound Eye user ID.
    pub fn consume(
        &self,
        raw_code: &str,
        callback_url: &str,
        allowlist: &CallbackAllowlist,
        now_secs: u64,
    ) -> Result<String, OAuthFlowError> {
        if raw_code.is_empty() {
            return Err(OAuthFlowError::InvalidHandoff);
        }
        let code_hash = hash_code(raw_code);
        let entry = self
            .entries_by_code_hash
            .lock()
            .map_err(|_| OAuthFlowError::StoreUnavailable)?
            .remove(&code_hash)
            .ok_or(OAuthFlowError::InvalidHandoff)?;

        if now_secs >= entry.expires_at_secs {
            return Err(OAuthFlowError::HandoffExpired);
        }
        // Consume before validating the supplied callback so even an unapproved/mismatched URL
        // cannot be used to probe the code and then retry the same code with the right URL.
        let callback_url = allowlist.require(callback_url)?;
        if callback_url != entry.callback_url {
            return Err(OAuthFlowError::CallbackMismatch);
        }
        Ok(entry.user_id)
    }
}

fn hash_code(raw_code: &str) -> String {
    Sha256::digest(raw_code.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
