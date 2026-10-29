//! Tests for signed JWT access tokens and refresh rotation (issue #44).

use king_sparkon_eye::auth::{
    AccessTokenClaims, TokenError, TokenManager, UserRole, ACCESS_TOKEN_TTL_SECS, EYE_TOKEN_AUDIENCE,
    EYE_TOKEN_ISSUER,
};

const SECRET: &[u8] = b"super-secret-key-32-bytes-minimum!!";
const KID: &str = "key-2026-v1";

#[test]
fn issuing_and_verifying_valid_access_token() {
    let manager = TokenManager::new(SECRET.to_vec(), KID);
    let now = 1_700_000_000;

    let pair = manager
        .issue_token_pair(
            "usr_test_123",
            UserRole::GateOperator,
            vec!["gate:identify".to_string()],
            Some("evt_test_festival".to_string()),
            now,
        )
        .expect("issued");

    assert_eq!(pair.token_type, "Bearer");
    assert_eq!(pair.expires_in_secs, ACCESS_TOKEN_TTL_SECS);

    let claims = manager.verify_access(&pair.access_token, now + 100).expect("valid");
    assert_eq!(claims.iss, EYE_TOKEN_ISSUER);
    assert_eq!(claims.aud, EYE_TOKEN_AUDIENCE);
    assert_eq!(claims.sub, "usr_test_123");
    assert_eq!(claims.role, UserRole::GateOperator);
    assert_eq!(claims.event_id.as_deref(), Some("evt_test_festival"));
}

#[test]
fn expired_access_token_is_rejected() {
    let manager = TokenManager::new(SECRET.to_vec(), KID);
    let now = 1_700_000_000;

    let pair = manager
        .issue_token_pair(
            "usr_test_123",
            UserRole::GateOperator,
            vec![],
            None,
            now,
        )
        .unwrap();

    // Verify after expiration time (now + ACCESS_TOKEN_TTL_SECS + 1)
    let res = manager.verify_access(&pair.access_token, now + ACCESS_TOKEN_TTL_SECS + 5);
    assert_eq!(res, Err(TokenError::Expired));
}

#[test]
fn tampered_token_is_rejected() {
    let manager = TokenManager::new(SECRET.to_vec(), KID);
    let now = 1_700_000_000;

    let pair = manager
        .issue_token_pair(
            "usr_test_123",
            UserRole::GateOperator,
            vec![],
            None,
            now,
        )
        .unwrap();

    let tampered = format!("{}extra", pair.access_token);
    assert_eq!(
        manager.verify_access(&tampered, now),
        Err(TokenError::InvalidSignature)
    );
}

#[test]
fn refresh_rotation_issues_new_pair_and_invalidates_old() {
    let manager = TokenManager::new(SECRET.to_vec(), KID);
    let now = 1_700_000_000;

    let pair1 = manager
        .issue_token_pair(
            "usr_test_123",
            UserRole::GateOperator,
            vec!["gate:identify".to_string()],
            None,
            now,
        )
        .unwrap();

    // Successful rotation
    let pair2 = manager
        .rotate_refresh_token(&pair1.refresh_token, now + 50)
        .expect("rotated");
    assert_ne!(pair1.refresh_token, pair2.refresh_token);

    // Verify new access token works
    let claims = manager.verify_access(&pair2.access_token, now + 60).unwrap();
    assert_eq!(claims.sub, "usr_test_123");
}

#[test]
fn refresh_token_replay_revokes_entire_family() {
    let manager = TokenManager::new(SECRET.to_vec(), KID);
    let now = 1_700_000_000;

    let pair1 = manager
        .issue_token_pair("usr_test_123", UserRole::GateOperator, vec![], None, now)
        .unwrap();

    // First rotation succeeds
    let pair2 = manager
        .rotate_refresh_token(&pair1.refresh_token, now + 10)
        .unwrap();

    // Attacker or replay attempts to reuse pair1.refresh_token!
    let replay_res = manager.rotate_refresh_token(&pair1.refresh_token, now + 20);
    assert_eq!(replay_res, Err(TokenError::ReplayDetected));

    // Even the legitimately rotated pair2 is now revoked!
    let revoked_res = manager.rotate_refresh_token(&pair2.refresh_token, now + 30);
    assert_eq!(revoked_res, Err(TokenError::ReplayDetected));
}
