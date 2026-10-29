//! Adversarial tests for Eye's Rust-owned authentication (issue #48).
//!
//! These use synthetic credentials only. No real provider, user or biometric data is involved.
//!
//! Tests marked `#[ignore]` assert correct behaviour that the current code does not yet provide.
//! Each names its tracking bug. Run them with `cargo test --test adversarial_auth_test -- --ignored`.
//! Remove the `#[ignore]` when the bug is fixed.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use king_sparkon_eye::auth::jwt::{AccessTokenClaims, sign_access_token, verify_access_token};
use king_sparkon_eye::auth::{
    API_KEY_PREFIX, ApiKeyError, ApiKeyService, AuthError, AuthService, EYE_TOKEN_AUDIENCE,
    EYE_TOKEN_ISSUER, MemoryApiKeyStore, MemoryIdentityStore, TokenError, TokenManager, UserRole,
};
use sha2::{Digest, Sha256};

const SECRET: &[u8] = b"adversarial-test-secret-32-bytes-min!!";
const OTHER_SECRET: &[u8] = b"a-different-secret-that-is-also-long!!";
const KID: &str = "kid-current";
const NOW: u64 = 1_700_000_000;
const NOW_MS: u64 = 1_700_000_000_000;

fn claims(now: u64) -> AccessTokenClaims {
    AccessTokenClaims {
        iss: EYE_TOKEN_ISSUER.to_string(),
        sub: "usr_adv_1".to_string(),
        aud: EYE_TOKEN_AUDIENCE.to_string(),
        exp: now + 900,
        iat: now,
        jti: "tok_adv_1".to_string(),
        role: UserRole::GateOperator,
        scopes: vec!["gate:identify".to_string()],
        event_id: Some("evt_adv".to_string()),
    }
}

fn signed(claims: &AccessTokenClaims) -> String {
    sign_access_token(claims, KID, SECRET).expect("signs")
}

/// Rebuilds a token with a different payload but the original header and signature.
fn with_payload(token: &str, payload: &serde_json::Value) -> String {
    let parts: Vec<&str> = token.split('.').collect();
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap());
    format!("{}.{}.{}", parts[0], body, parts[2])
}

/// Pre-#100 signing construction, kept only to prove it is rejected now.
fn signature_for(header_b64: &str, claims_b64: &str, secret: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(secret);
    h.update(b":");
    h.update(header_b64.as_bytes());
    h.update(b".");
    h.update(claims_b64.as_bytes());
    URL_SAFE_NO_PAD.encode(h.finalize())
}

// ---------- JWT: tampering and key handling ----------

#[test]
fn payload_tampering_is_rejected() {
    let token = signed(&claims(NOW));
    let mut tampered = serde_json::to_value(claims(NOW)).unwrap();
    tampered["role"] = "ADMIN".into();
    tampered["scopes"] = serde_json::json!(["admin:all"]);
    let forged = with_payload(&token, &tampered);

    assert_eq!(
        verify_access_token(&forged, KID, SECRET, NOW + 1),
        Err(TokenError::InvalidSignature)
    );
}

#[test]
fn token_signed_with_another_secret_is_rejected() {
    let token = sign_access_token(&claims(NOW), KID, OTHER_SECRET).unwrap();
    assert_eq!(
        verify_access_token(&token, KID, SECRET, NOW + 1),
        Err(TokenError::InvalidSignature)
    );
}

#[test]
fn stripped_or_empty_signature_is_rejected() {
    let token = signed(&claims(NOW));
    let parts: Vec<&str> = token.split('.').collect();
    let stripped = format!("{}.{}.", parts[0], parts[1]);
    assert_eq!(
        verify_access_token(&stripped, KID, SECRET, NOW + 1),
        Err(TokenError::InvalidSignature)
    );
}

#[test]
fn malformed_tokens_are_rejected_without_panicking() {
    for bad in [
        "",
        "a",
        "a.b",
        "a.b.c.d",
        "...",
        "!!.!!.!!",
        "e30.e30.not-base64-!!",
    ] {
        assert_eq!(
            verify_access_token(bad, KID, SECRET, NOW + 1),
            Err(TokenError::InvalidSignature),
            "input {bad:?}"
        );
    }
}

#[test]
fn key_rotation_invalidates_tokens_issued_under_old_kid() {
    let token = signed(&claims(NOW));
    assert_eq!(
        verify_access_token(&token, "kid-rotated", SECRET, NOW + 1),
        Err(TokenError::InvalidSignature)
    );
    assert!(verify_access_token(&token, KID, SECRET, NOW + 1).is_ok());
}

#[test]
fn wrong_issuer_and_audience_are_rejected_even_with_valid_signature() {
    let mut bad_iss = claims(NOW);
    bad_iss.iss = "https://evil.example".to_string();
    let token = signed(&bad_iss);
    assert_eq!(
        verify_access_token(&token, KID, SECRET, NOW + 1),
        Err(TokenError::InvalidClaims)
    );

    let mut bad_aud = claims(NOW);
    bad_aud.aud = "some-other-service".to_string();
    let token = signed(&bad_aud);
    assert_eq!(
        verify_access_token(&token, KID, SECRET, NOW + 1),
        Err(TokenError::InvalidClaims)
    );
}

#[test]
fn expired_token_is_rejected() {
    let token = signed(&claims(NOW));
    assert_eq!(
        verify_access_token(&token, KID, SECRET, NOW + 901),
        Err(TokenError::Expired)
    );
}

#[test]
fn token_is_rejected_exactly_at_expiry() {
    let token = signed(&claims(NOW));
    assert_eq!(
        verify_access_token(&token, KID, SECRET, NOW + 900),
        Err(TokenError::Expired)
    );
}

#[test]
fn token_issued_in_the_future_is_rejected() {
    let token = signed(&claims(NOW + 10_000));
    assert_eq!(
        verify_access_token(&token, KID, SECRET, NOW),
        Err(TokenError::NotYetValid)
    );
}

#[test]
fn non_hs256_algorithm_header_is_rejected() {
    let header = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({"alg": "RS256", "typ": "JWT", "kid": KID})).unwrap(),
    );
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims(NOW)).unwrap());
    let sig = signature_for(&header, &body, SECRET);
    let token = format!("{header}.{body}.{sig}");
    assert_eq!(
        verify_access_token(&token, KID, SECRET, NOW + 1),
        Err(TokenError::UnsupportedAlgorithm)
    );
}

// ---------- JWT: refresh rotation and replay ----------

#[test]
fn reused_refresh_token_revokes_the_whole_session_family() {
    let manager = TokenManager::new(SECRET.to_vec(), KID);
    let first = manager
        .issue_token_pair(
            "usr_adv_2",
            UserRole::GateOperator,
            vec!["gate:identify".into()],
            Some("evt_adv".into()),
            NOW,
        )
        .unwrap();

    let second = manager
        .rotate_refresh_token(&first.refresh_token, NOW + 10)
        .expect("first rotation");

    // Replaying the already-used token is detected and revokes the family.
    assert_eq!(
        manager.rotate_refresh_token(&first.refresh_token, NOW + 20),
        Err(TokenError::ReplayDetected)
    );
    // The legitimate newer token in the same family must no longer work.
    assert!(
        manager
            .rotate_refresh_token(&second.refresh_token, NOW + 30)
            .is_err()
    );
}

#[test]
fn unknown_refresh_token_is_rejected() {
    let manager = TokenManager::new(SECRET.to_vec(), KID);
    assert_eq!(
        manager.rotate_refresh_token("refresh_does_not_exist", NOW),
        Err(TokenError::RefreshTokenNotFound)
    );
}

// ---------- JWT: secret exposure ----------

#[test]
fn token_pair_debug_output_contains_no_token() {
    let manager = TokenManager::new(SECRET.to_vec(), KID);
    let pair = manager
        .issue_token_pair("usr_adv_3", UserRole::GateOperator, vec![], None, NOW)
        .unwrap();
    let debug = format!("{pair:?}");
    assert!(!debug.contains(&pair.access_token));
    assert!(!debug.contains(&pair.refresh_token));
}

// ---------- API keys ----------

fn service() -> ApiKeyService<MemoryApiKeyStore> {
    ApiKeyService::new(MemoryApiKeyStore::new(), "live")
}

#[test]
fn wrong_key_with_correct_prefix_is_rejected() {
    let svc = service();
    let created = svc
        .create_key(
            "owner_a",
            "scanner",
            vec!["gate:identify".into()],
            None,
            NOW_MS,
        )
        .unwrap();
    let mut forged = created.raw_key.clone();
    let last = forged.pop().unwrap();
    forged.push(if last == 'a' { 'b' } else { 'a' });

    assert_eq!(
        svc.verify_key(&forged, None, NOW_MS),
        Err(ApiKeyError::InvalidKey)
    );
}

#[test]
fn malformed_and_foreign_keys_are_rejected_identically() {
    let svc = service();
    svc.create_key(
        "owner_a",
        "scanner",
        vec!["gate:identify".into()],
        None,
        NOW_MS,
    )
    .unwrap();

    // Enumeration resistance: unknown and malformed keys produce the same error.
    for candidate in [
        "",
        "kse_",
        "not-a-key",
        &format!(
            "{API_KEY_PREFIX}live_0000000000000000000000000000000000000000000000000000000000000000"
        ),
    ] {
        assert_eq!(
            svc.verify_key(candidate, None, NOW_MS),
            Err(ApiKeyError::InvalidKey),
            "{candidate:?}"
        );
    }
}

#[test]
fn revoked_key_is_rejected() {
    let svc = service();
    let created = svc
        .create_key(
            "owner_a",
            "scanner",
            vec!["gate:identify".into()],
            None,
            NOW_MS,
        )
        .unwrap();
    svc.revoke_key(&created.record.key_id).unwrap();
    assert_eq!(
        svc.verify_key(&created.raw_key, None, NOW_MS),
        Err(ApiKeyError::Revoked)
    );
}

#[test]
fn expired_key_is_rejected() {
    let svc = service();
    let created = svc
        .create_key(
            "owner_a",
            "short",
            vec!["gate:identify".into()],
            Some(1_000),
            NOW_MS,
        )
        .unwrap();
    assert_eq!(
        svc.verify_key(&created.raw_key, None, NOW_MS + 1_001),
        Err(ApiKeyError::Expired)
    );
}

#[test]
fn key_without_required_scope_is_rejected() {
    let svc = service();
    let created = svc
        .create_key(
            "owner_a",
            "scanner",
            vec!["gate:identify".into()],
            None,
            NOW_MS,
        )
        .unwrap();
    assert_eq!(
        svc.verify_key(&created.raw_key, Some("enrollment:write"), NOW_MS),
        Err(ApiKeyError::InsufficientScope)
    );
    assert!(
        svc.verify_key(&created.raw_key, Some("gate:identify"), NOW_MS)
            .is_ok()
    );
}

#[test]
fn admin_all_scope_satisfies_any_required_scope() {
    let svc = service();
    let created = svc
        .create_key(
            "owner_admin",
            "admin",
            vec!["admin:all".into()],
            None,
            NOW_MS,
        )
        .unwrap();
    assert!(
        svc.verify_key(&created.raw_key, Some("enrollment:write"), NOW_MS)
            .is_ok()
    );
    assert!(
        svc.verify_key(&created.raw_key, Some("gate:identify"), NOW_MS)
            .is_ok()
    );
}

#[test]
fn revoking_one_owners_key_does_not_affect_another_owner() {
    let svc = service();
    let a = svc
        .create_key("owner_a", "a", vec!["gate:identify".into()], None, NOW_MS)
        .unwrap();
    let b = svc
        .create_key("owner_b", "b", vec!["gate:identify".into()], None, NOW_MS)
        .unwrap();

    svc.revoke_key(&a.record.key_id).unwrap();

    assert_eq!(
        svc.verify_key(&a.raw_key, None, NOW_MS),
        Err(ApiKeyError::Revoked)
    );
    let still_valid = svc
        .verify_key(&b.raw_key, None, NOW_MS)
        .expect("owner_b unaffected");
    assert_eq!(still_valid.owner_id, "owner_b");
}

#[test]
fn stored_record_serialization_contains_no_raw_key() {
    let svc = service();
    let created = svc
        .create_key(
            "owner_a",
            "scanner",
            vec!["gate:identify".into()],
            None,
            NOW_MS,
        )
        .unwrap();
    let json = serde_json::to_string(&created.record).unwrap();
    assert!(!json.contains(&created.raw_key));
    assert!(!json.contains(created.raw_key.trim_start_matches(API_KEY_PREFIX)));
}

#[test]
fn created_key_debug_output_contains_no_raw_key() {
    let svc = service();
    let created = svc
        .create_key(
            "owner_a",
            "scanner",
            vec!["gate:identify".into()],
            None,
            NOW_MS,
        )
        .unwrap();
    assert!(!format!("{created:?}").contains(&created.raw_key));
}

// ---------- Passwords ----------

#[test]
fn wrong_password_and_unknown_email_return_the_same_error() {
    let auth = AuthService::new(MemoryIdentityStore::new());
    auth.register(
        "gate.operator@example.test",
        "correct-horse-battery",
        UserRole::GateOperator,
        vec![],
        NOW_MS,
    )
    .unwrap();

    let wrong_password = auth.authenticate("gate.operator@example.test", "wrong-password-value");
    let unknown_email = auth.authenticate("nobody@example.test", "correct-horse-battery");

    assert_eq!(wrong_password.unwrap_err(), AuthError::InvalidCredentials);
    assert_eq!(unknown_email.unwrap_err(), AuthError::InvalidCredentials);
}

#[test]
fn correct_password_authenticates() {
    let auth = AuthService::new(MemoryIdentityStore::new());
    auth.register(
        "gate.operator@example.test",
        "correct-horse-battery",
        UserRole::GateOperator,
        vec![],
        NOW_MS,
    )
    .unwrap();
    assert!(
        auth.authenticate("gate.operator@example.test", "correct-horse-battery")
            .is_ok()
    );
}

// ---------- Gaps not covered here (recorded for #48) ----------
// - Password brute-force / rate limiting: not implemented.
// - Verification and reset token expiry: no such flows exist yet.
// - Logout: no logout operation exists.
// - Google OAuth callback cases: no HTTP route is mounted (#93). Library-level cases are in
//   tests/google_oidc_test.rs.
// - Route-policy and cross-tenant HTTP tests: no authenticated routes are mounted.

#[test]
fn token_with_legacy_hash_construction_is_rejected() {
    // Issue #100: signatures from the pre-HMAC construction must no longer verify.
    let header = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({"alg": "HS256", "typ": "JWT", "kid": KID})).unwrap(),
    );
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims(NOW)).unwrap());
    let sig = signature_for(&header, &body, SECRET);
    let token = format!("{header}.{body}.{sig}");
    assert_eq!(
        verify_access_token(&token, KID, SECRET, NOW + 1),
        Err(TokenError::InvalidSignature)
    );
}

#[test]
fn token_with_standard_hmac_sha256_signature_is_accepted() {
    // Independent check of the signing scheme: RFC 2104 HMAC-SHA256 computed with ring directly.
    use ring::hmac;
    let header = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&serde_json::json!({"alg": "HS256", "typ": "JWT", "kid": KID})).unwrap(),
    );
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims(NOW)).unwrap());
    let key = hmac::Key::new(hmac::HMAC_SHA256, SECRET);
    let tag = hmac::sign(&key, format!("{header}.{body}").as_bytes());
    let token = format!("{header}.{body}.{}", URL_SAFE_NO_PAD.encode(tag.as_ref()));
    assert!(verify_access_token(&token, KID, SECRET, NOW + 1).is_ok());
}
