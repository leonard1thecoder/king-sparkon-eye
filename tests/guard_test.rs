//! Tests for identity, tenant, scope and event boundary enforcement (issue #47).

use king_sparkon_eye::auth::{
    AccessTokenClaims, ApiKeyRecord, BoundaryError, CallerIdentity, RouteGuard, UserRole,
    EYE_TOKEN_AUDIENCE, EYE_TOKEN_ISSUER,
};

fn sample_user_caller(role: UserRole, scopes: Vec<&str>, event_id: Option<&str>) -> CallerIdentity {
    CallerIdentity::User(AccessTokenClaims {
        iss: EYE_TOKEN_ISSUER.to_string(),
        sub: "usr_operator_001".to_string(),
        aud: EYE_TOKEN_AUDIENCE.to_string(),
        exp: 1_700_001_000,
        iat: 1_700_000_000,
        jti: "tok_test_001".to_string(),
        role,
        scopes: scopes.into_iter().map(|s| s.to_string()).collect(),
        event_id: event_id.map(|e| e.to_string()),
    })
}

fn sample_api_key_caller(scopes: Vec<&str>) -> CallerIdentity {
    CallerIdentity::ApiKey(ApiKeyRecord {
        key_id: "kse_live_testkey".to_string(),
        key_hash: "hash".to_string(),
        salt: "salt".to_string(),
        owner_id: "scanner_device_01".to_string(),
        label: "Gate Scanner".to_string(),
        scopes: scopes.into_iter().map(|s| s.to_string()).collect(),
        expires_at_ms: None,
        revoked: false,
        created_at_ms: 1_700_000_000_000,
    })
}

#[test]
fn enrollment_route_requires_enrollment_write_scope() {
    let authorized_caller = sample_user_caller(UserRole::GateOperator, vec!["enrollment:write"], None);
    assert_eq!(RouteGuard::authorize_enrollment(&authorized_caller), Ok(()));

    let unauthorized_caller = sample_user_caller(UserRole::GateOperator, vec!["gate:identify"], None);
    assert_eq!(
        RouteGuard::authorize_enrollment(&unauthorized_caller),
        Err(BoundaryError::ForbiddenScope)
    );
}

#[test]
fn gate_identification_enforces_event_scoping_and_prevents_cross_event_access() {
    let caller_event_a = sample_user_caller(
        UserRole::GateOperator,
        vec!["gate:identify"],
        Some("evt_summer_fest_2026"),
    );

    // Same event matches
    assert_eq!(
        RouteGuard::authorize_gate_identification(&caller_event_a, "evt_summer_fest_2026"),
        Ok(())
    );

    // Cross-event access to event_b is strictly rejected
    assert_eq!(
        RouteGuard::authorize_gate_identification(&caller_event_a, "evt_winter_gala_2026"),
        Err(BoundaryError::CrossEventAccessDenied)
    );
}

#[test]
fn admin_bypasses_specific_event_restriction_and_has_all_scopes() {
    let admin_caller = sample_user_caller(UserRole::Admin, vec![], None);

    assert_eq!(RouteGuard::authorize_enrollment(&admin_caller), Ok(()));
    assert_eq!(
        RouteGuard::authorize_gate_identification(&admin_caller, "evt_any_event"),
        Ok(())
    );
    assert_eq!(RouteGuard::authorize_admin(&admin_caller), Ok(()));
}

#[test]
fn api_key_with_matching_scope_is_authorized() {
    let key_caller = sample_api_key_caller(vec!["enrollment:write"]);
    assert_eq!(RouteGuard::authorize_enrollment(&key_caller), Ok(()));
    assert_eq!(
        RouteGuard::authorize_admin(&key_caller),
        Err(BoundaryError::ForbiddenScope)
    );
}
