//! Identity storage and password authentication tests (issue #43).

use king_sparkon_eye::auth::{AuthError, AuthService, MemoryIdentityStore, UserRole};

#[test]
fn registering_valid_user_creates_profile_without_exposing_secrets() {
    let store = MemoryIdentityStore::new();
    let service = AuthService::new(store);

    let profile = service
        .register(
            "operator@kingsparkon.internal",
            "StrongPassword2026!",
            UserRole::GateOperator,
            vec!["gate:identify".to_string()],
            1_700_000_000_000,
        )
        .expect("registered");

    assert_eq!(profile.email, "operator@kingsparkon.internal");
    assert_eq!(profile.role, UserRole::GateOperator);
    assert!(profile.user_id.starts_with("usr_"));
    assert!(profile.is_active);
}

#[test]
fn weak_passwords_are_rejected() {
    let store = MemoryIdentityStore::new();
    let service = AuthService::new(store);

    let res = service.register(
        "operator@kingsparkon.internal",
        "short",
        UserRole::GateOperator,
        vec![],
        1_700_000_000_000,
    );

    assert_eq!(res, Err(AuthError::WeakPassword));
}

#[test]
fn duplicate_emails_are_rejected() {
    let store = MemoryIdentityStore::new();
    let service = AuthService::new(store);

    let _ = service
        .register(
            "admin@kingsparkon.internal",
            "ComplexPassword123!",
            UserRole::Admin,
            vec!["admin:all".to_string()],
            1_700_000_000_000,
        )
        .unwrap();

    let res = service.register(
        "admin@kingsparkon.internal",
        "AnotherPassword456!",
        UserRole::Admin,
        vec![],
        1_700_000_000_000,
    );

    assert_eq!(res, Err(AuthError::UserAlreadyExists));
}

#[test]
fn authentication_succeeds_with_correct_credentials() {
    let store = MemoryIdentityStore::new();
    let service = AuthService::new(store);

    let _ = service
        .register(
            "worker@kingsparkon.internal",
            "CorrectPassword2026!",
            UserRole::GateOperator,
            vec!["gate:identify".to_string()],
            1_700_000_000_000,
        )
        .unwrap();

    let auth = service
        .authenticate("worker@kingsparkon.internal", "CorrectPassword2026!")
        .expect("authenticated");

    assert_eq!(auth.email, "worker@kingsparkon.internal");
}

#[test]
fn authentication_fails_uniformly_for_wrong_password_or_missing_user() {
    let store = MemoryIdentityStore::new();
    let service = AuthService::new(store);

    let _ = service
        .register(
            "worker@kingsparkon.internal",
            "CorrectPassword2026!",
            UserRole::GateOperator,
            vec![],
            1_700_000_000_000,
        )
        .unwrap();

    // Wrong password
    let res_wrong_pass = service.authenticate("worker@kingsparkon.internal", "WrongPassword!");
    assert_eq!(res_wrong_pass, Err(AuthError::InvalidCredentials));

    // Non-existent email returns exact same error (enumeration resistance)
    let res_missing_user = service.authenticate("nobody@kingsparkon.internal", "SomePassword!");
    assert_eq!(res_missing_user, Err(AuthError::InvalidCredentials));
}
