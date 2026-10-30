//! Unit tests for the password hashing module (issue #135).

use king_sparkon_eye::auth::hash_password as legacy_hash;
use king_sparkon_eye::auth::password::{
    MAX_BCRYPT_PASSWORD_BYTES, PasswordError, Verification, hash_password_with_cost,
    is_bcrypt_hash, spend_equivalent_time, verify_stored_password,
};

// Cost 4 keeps these tests fast. Production uses BCRYPT_COST (12).
const TEST_COST: u32 = 4;

#[test]
fn bcrypt_hash_verifies_only_the_right_password() {
    let hash = hash_password_with_cost("correct-horse-1", TEST_COST).unwrap();
    assert!(is_bcrypt_hash(&hash));
    assert_eq!(
        verify_stored_password("correct-horse-1", &hash, ""),
        Verification::Valid {
            needs_rehash: false
        }
    );
    assert_eq!(
        verify_stored_password("wrong-horse-1", &hash, ""),
        Verification::Invalid
    );
}

#[test]
fn bcrypt_hashes_are_salted() {
    let first = hash_password_with_cost("same-password", TEST_COST).unwrap();
    let second = hash_password_with_cost("same-password", TEST_COST).unwrap();
    assert_ne!(first, second);
}

#[test]
fn passwords_over_the_limit_cannot_be_hashed_or_verified_with_bcrypt() {
    let limit = "a".repeat(MAX_BCRYPT_PASSWORD_BYTES);
    let longer = format!("{limit}b");
    assert_eq!(
        hash_password_with_cost(&longer, TEST_COST),
        Err(PasswordError::TooLong)
    );
    // The longest accepted password verifies; a longer one with the same prefix must not.
    let hash = hash_password_with_cost(&limit, TEST_COST).unwrap();
    assert_eq!(
        verify_stored_password(&limit, &hash, ""),
        Verification::Valid {
            needs_rehash: false
        }
    );
    assert_eq!(
        verify_stored_password(&longer, &hash, ""),
        Verification::Invalid
    );
}

#[test]
fn legacy_sha256_hash_verifies_and_asks_for_rehash() {
    let salt = "0123456789abcdef";
    let stored = legacy_hash("legacy-password-1", salt);
    assert!(!is_bcrypt_hash(&stored));
    assert_eq!(
        verify_stored_password("legacy-password-1", &stored, salt),
        Verification::Valid { needs_rehash: true }
    );
    assert_eq!(
        verify_stored_password("not-the-password", &stored, salt),
        Verification::Invalid
    );
}

#[test]
fn legacy_hash_over_72_bytes_is_valid_but_not_rehashed() {
    let salt = "fedcba9876543210";
    let password = "p".repeat(100);
    let stored = legacy_hash(&password, salt);
    assert_eq!(
        verify_stored_password(&password, &stored, salt),
        Verification::Valid {
            needs_rehash: false
        }
    );
}

#[test]
fn malformed_stored_values_never_verify() {
    assert_eq!(
        verify_stored_password("anything-1", "", ""),
        Verification::Invalid
    );
    assert_eq!(
        verify_stored_password("anything-1", "$2b$12$short", ""),
        Verification::Invalid
    );
    assert_eq!(
        verify_stored_password("anything-1", &"0".repeat(64), "salt"),
        Verification::Invalid
    );
}

#[test]
fn timing_equaliser_runs_without_panicking() {
    spend_equivalent_time("any-password-1");
    spend_equivalent_time(&"x".repeat(200));
}
