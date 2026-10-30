//! Password hashing for login (issue #135).
//!
//! New hashes are bcrypt (cost 12). Stored rows created before this change hold salted SHA-256
//! digests; those are still verified, and rehashed to bcrypt on a correct login.
//!
//! bcrypt reads only the first 72 bytes of a password. The non-truncating API is used so a
//! longer password can never match a stored hash by accident.

use std::sync::OnceLock;

use crate::auth::hash_password as legacy_sha256_hex;

/// bcrypt work factor for new hashes. Each hash takes roughly 250 ms at this cost.
pub const BCRYPT_COST: u32 = bcrypt::DEFAULT_COST;

/// Longest password the non-truncating bcrypt API accepts. bcrypt reads 72 bytes including a
/// terminator, so the crate rejects anything of 72 bytes or more.
pub const MAX_BCRYPT_PASSWORD_BYTES: usize = 71;

/// Outcome of checking a password against a stored hash.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verification {
    /// The password does not match, or the stored hash is malformed.
    Invalid,
    /// The password matches. `needs_rehash` is true for a legacy hash that can be upgraded.
    Valid { needs_rehash: bool },
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PasswordError {
    #[error("password is longer than the bcrypt limit")]
    TooLong,
    #[error("password hashing failed")]
    Hashing,
}

/// Hashes a password with bcrypt at the given cost. Passwords longer than 71 bytes are rejected.
pub fn hash_password_with_cost(password: &str, cost: u32) -> Result<String, PasswordError> {
    if password.len() > MAX_BCRYPT_PASSWORD_BYTES {
        return Err(PasswordError::TooLong);
    }
    bcrypt::non_truncating_hash(password, cost).map_err(|error| match error {
        bcrypt::BcryptError::Truncation(_) => PasswordError::TooLong,
        _ => PasswordError::Hashing,
    })
}

/// Hashes a password with the production work factor.
pub fn hash_password(password: &str) -> Result<String, PasswordError> {
    hash_password_with_cost(password, BCRYPT_COST)
}

/// True for bcrypt hashes (`$2a$`, `$2b$`, `$2x$`, `$2y$`).
pub fn is_bcrypt_hash(stored: &str) -> bool {
    stored.len() == 60 && stored.starts_with("$2")
}

/// Checks a password against a stored hash.
///
/// Legacy rows are salted SHA-256 hex digests. They are compared in constant time and reported
/// with `needs_rehash` when the password fits in bcrypt's limit.
pub fn verify_stored_password(password: &str, stored: &str, salt: &str) -> Verification {
    if is_bcrypt_hash(stored) {
        return match bcrypt::non_truncating_verify(password, stored) {
            Ok(true) => Verification::Valid {
                needs_rehash: false,
            },
            _ => Verification::Invalid,
        };
    }
    let candidate = legacy_sha256_hex(password, salt);
    if constant_time_eq(candidate.as_bytes(), stored.as_bytes()) {
        Verification::Valid {
            needs_rehash: password.len() <= MAX_BCRYPT_PASSWORD_BYTES,
        }
    } else {
        Verification::Invalid
    }
}

/// Runs a bcrypt verification against a fixed hash. Call this for unknown accounts so that
/// response time does not reveal whether an account exists.
pub fn spend_equivalent_time(password: &str) {
    static DUMMY: OnceLock<String> = OnceLock::new();
    let dummy = DUMMY.get_or_init(|| {
        bcrypt::hash("eye-login-timing-equaliser", BCRYPT_COST).unwrap_or_default()
    });
    let _ = bcrypt::non_truncating_verify(password, dummy);
}

/// Compares two byte strings without returning early on the first difference.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}
