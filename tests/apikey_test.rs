//! Tests for API key creation, hashed storage, scope enforcement and revocation (issue #46).

use king_sparkon_eye::auth::{ApiKeyError, ApiKeyService, MemoryApiKeyStore, API_KEY_PREFIX};

#[test]
fn creating_key_returns_formatted_key_and_persists_hash_only() {
    let store = MemoryApiKeyStore::new();
    let service = ApiKeyService::new(store, "live");
    let now = 1_700_000_000_000;

    let created = service
        .create_key(
            "dev_001",
            "Gate Scanner Key",
            vec!["gate:identify".to_string()],
            Some(3600_000),
            now,
        )
        .expect("created");

    assert!(created.raw_key.starts_with(&format!("{API_KEY_PREFIX}live_")));
    assert_ne!(created.raw_key, created.record.key_hash);
    assert_eq!(created.record.owner_id, "dev_001");
    assert_eq!(created.record.scopes, vec!["gate:identify".to_string()]);
    assert!(!created.record.revoked);
}

#[test]
fn verifying_valid_key_with_matching_scope_succeeds() {
    let store = MemoryApiKeyStore::new();
    let service = ApiKeyService::new(store, "live");
    let now = 1_700_000_000_000;

    let created = service
        .create_key(
            "dev_001",
            "Scanner 1",
            vec!["gate:identify".to_string(), "enrollment:write".to_string()],
            None,
            now,
        )
        .unwrap();

    let verified = service
        .verify_key(&created.raw_key, Some("gate:identify"), now + 1000)
        .expect("verified");

    assert_eq!(verified.key_id, created.record.key_id);
}

#[test]
fn insufficient_scope_is_rejected() {
    let store = MemoryApiKeyStore::new();
    let service = ApiKeyService::new(store, "live");
    let now = 1_700_000_000_000;

    let created = service
        .create_key(
            "dev_001",
            "Enrollment Device",
            vec!["enrollment:write".to_string()],
            None,
            now,
        )
        .unwrap();

    let res = service.verify_key(&created.raw_key, Some("gate:identify"), now + 1000);
    assert_eq!(res, Err(ApiKeyError::InsufficientScope));
}

#[test]
fn revoked_key_is_rejected() {
    let store = MemoryApiKeyStore::new();
    let service = ApiKeyService::new(store, "live");
    let now = 1_700_000_000_000;

    let created = service
        .create_key("dev_001", "Temp Key", vec!["gate:identify".to_string()], None, now)
        .unwrap();

    // Revoke by key_id
    service.revoke_key(&created.record.key_id).expect("revoked");

    let res = service.verify_key(&created.raw_key, None, now + 500);
    assert_eq!(res, Err(ApiKeyError::Revoked));
}

#[test]
fn expired_key_is_rejected() {
    let store = MemoryApiKeyStore::new();
    let service = ApiKeyService::new(store, "live");
    let now = 1_700_000_000_000;

    let created = service
        .create_key(
            "dev_001",
            "Short-lived Key",
            vec![],
            Some(1000), // 1 second TTL
            now,
        )
        .unwrap();

    // Check after expiry (now + 2000)
    let res = service.verify_key(&created.raw_key, None, now + 2000);
    assert_eq!(res, Err(ApiKeyError::Expired));
}
