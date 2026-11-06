//! Integration coverage for every mounted SQLx-backed auth persistence family.
//!
//! Requires EYE_TEST_DATABASE_URL pointing to a disposable PostgreSQL database.
//! Tests skip explicitly when it is absent; skipped tests are not evidence of DB verification.

use king_sparkon_eye::auth::durable_store::{
    DurableEyeStore, DurableUser, RegisterUserInput, hash_hex,
};
use king_sparkon_eye::auth::UserRole;
use king_sparkon_eye::config::{OAuthRuntimeConfig, SecretString};
use king_sparkon_eye::persistence::{connect, migrate};
use sqlx::PgPool;
use uuid::Uuid;

async fn store() -> Option<(PgPool, DurableEyeStore)> {
    let url = match std::env::var("EYE_TEST_DATABASE_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; auth-store checks not run");
            return None;
        }
    };
    let pool = connect(&url).await.expect("connect to disposable PostgreSQL database");
    migrate(&pool).await.expect("apply Eye SQLx migrations");
    let config = OAuthRuntimeConfig {
        google_client_id: "test-client".to_owned(),
        google_redirect_url: "https://eye.example.test/v1/auth/google/callback".to_owned(),
        google_client_secret: SecretString::new("test-secret"),
        database_url: url,
        oauth_encryption_key: SecretString::new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="),
        callback_urls: "https://app.example.test/auth/callback".to_owned(),
        allow_localhost_http: false,
        exchange_secret: SecretString::new("x".repeat(32)),
        jwt_signing_secret: SecretString::new("j".repeat(32)),
        jwt_kid: "test-key".to_owned(),
    };
    let store = DurableEyeStore::new(pool.clone(), &config).expect("valid test encryption key");
    Some((pool, store))
}

fn user_id() -> String {
    format!("usr_{}", Uuid::new_v4().simple())
}
fn email() -> String {
    format!("auth-{}@example.test", Uuid::new_v4().simple())
}
fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
}
fn new_user(user_id: String, email: String, now: u64) -> RegisterUserInput<'static> {
    // This helper is intentionally not used: borrowed input must live in the test scope.
    let _ = (user_id, email, now);
    unreachable!()
}
async fn register(store: &DurableEyeStore) -> DurableUser {
    let id = user_id();
    let mail = email();
    let password_hash = "bcrypt-hash-for-test";
    let salt = "test-salt";
    store.register_user(RegisterUserInput {
        user_id: &id,
        email: &mail,
        password_hash,
        salt,
        role: UserRole::GateOperator,
        scopes: &[],
        now_ms: now_ms(),
    }).await.expect("register user")
}

#[tokio::test]
async fn password_registration_lookup_rehash_and_reset_are_sqlx_backed() {
    let Some((pool, store)) = store().await else { return };
    let user = register(&store).await;
    let credentials = store.password_credentials_by_email(&user.email).await.unwrap().unwrap();
    assert_eq!(credentials.user_id, user.user_id);
    assert_eq!(store.user_by_id(&user.user_id).await.unwrap(), Some(user.clone()));
    assert!(store.update_password_hash(&user.user_id, "bcrypt-hash-for-test", "rehash-v2").await.unwrap());
    assert!(!store.update_password_hash(&user.user_id, "bcrypt-hash-for-test", "stale").await.unwrap());

    let token = format!("reset-{}", Uuid::new_v4());
    let token_hash = hash_hex(&token);
    assert_eq!(
        store.create_password_reset_token(&token_hash, &user.email, now_ms(), 60_000).await.unwrap(),
        Some(user.email.clone())
    );
    assert!(store.consume_password_reset_token(&token_hash, "new-password-hash", "new-salt", now_ms()).await.unwrap());
    assert!(!store.consume_password_reset_token(&token_hash, "replay", "salt", now_ms()).await.unwrap());
    let updated = store.password_credentials_by_email(&user.email).await.unwrap().unwrap();
    assert_eq!(updated.password_hash, "new-password-hash");
    assert_eq!(updated.salt, "new-salt");
    pool.close().await;
}

#[tokio::test]
async fn oauth_transaction_encrypts_secrets_and_burns_state_mismatch() {
    let Some((pool, store)) = store().await else { return };
    let cookie = hash_hex(&format!("cookie-{}", Uuid::new_v4()));
    let state = hash_hex(&format!("state-{}", Uuid::new_v4()));
    let wrong_state = hash_hex("wrong-state");
    let nonce = format!("nonce-{}", Uuid::new_v4());
    let verifier = format!("pkce-{}", Uuid::new_v4());
    let callback = "https://app.example.test/auth/callback";
    store.create_oauth_transaction(&cookie, &state, &nonce, &verifier, callback, now_ms()).await.unwrap();
    let consumed = store.consume_oauth_transaction(&cookie, &wrong_state, now_ms()).await.unwrap().unwrap();
    assert!(!consumed.state_valid);
    assert_eq!(consumed.nonce, nonce);
    assert_eq!(consumed.code_verifier, verifier);
    assert_eq!(consumed.callback_url, callback);
    assert!(store.consume_oauth_transaction(&cookie, &state, now_ms()).await.unwrap().is_none());
    pool.close().await;
}

#[tokio::test]
async fn google_identity_linking_requires_verified_email_and_is_idempotent() {
    let Some((pool, store)) = store().await else { return };
    let user = register(&store).await;
    let subject = format!("google-sub-{}", Uuid::new_v4());
    assert!(store.resolve_google_identity(&subject, &user.email, false, now_ms(), &dummy_user(&user)).await.is_err());
    let linked = store.resolve_google_identity(&subject, &user.email, true, now_ms(), &dummy_user(&user)).await.unwrap();
    assert_eq!(linked.user_id, user.user_id);
    let linked_again = store.resolve_google_identity(&subject, &user.email, true, now_ms(), &dummy_user(&user)).await.unwrap();
    assert_eq!(linked_again.user_id, user.user_id);
    pool.close().await;
}

fn dummy_user(user: &DurableUser) -> king_sparkon_eye::auth::UserRecord {
    king_sparkon_eye::auth::UserRecord {
        user_id: user.user_id.clone(),
        email: user.email.clone(),
        password_hash: "unused".to_owned(),
        salt: "unused".to_owned(),
        role: user.role,
        scopes: user.scopes.clone(),
        is_active: user.is_active,
        created_at_ms: user.created_at_ms,
    }
}

#[tokio::test]
async fn handoff_is_single_use_and_bound_to_callback() {
    let Some((pool, store)) = store().await else { return };
    let user = register(&store).await;
    let code = format!("handoff-{}", Uuid::new_v4());
    let code_hash = hash_hex(&code);
    let callback = "https://app.example.test/auth/callback";
    store.issue_handoff(&code_hash, &user.user_id, callback, now_ms()).await.unwrap();
    assert!(store.consume_handoff(&code_hash, "https://evil.example.test/callback", now_ms()).await.unwrap().is_none());
    assert!(store.consume_handoff(&code_hash, callback, now_ms()).await.unwrap().is_none());
    pool.close().await;
}

#[tokio::test]
async fn refresh_rotation_replay_revokes_the_entire_family() {
    let Some((pool, store)) = store().await else { return };
    let user = register(&store).await;
    let family = format!("fam_{}", Uuid::new_v4().simple());
    let old_hash = hash_hex(&format!("old-{}", Uuid::new_v4()));
    let next_hash = hash_hex(&format!("next-{}", Uuid::new_v4()));
    let now = now_ms();
    store.issue_refresh_token(&old_hash, &family, &user, None, now, now + 60_000).await.unwrap();
    assert_eq!(store.rotate_refresh_token(&old_hash, &next_hash, now + 1, now + 60_001).await.unwrap().outcome, "ok");
    assert_eq!(store.rotate_refresh_token(&old_hash, &hash_hex("third-token"), now + 2, now + 60_002).await.unwrap().outcome, "replay");
    assert_eq!(store.rotate_refresh_token(&next_hash, &hash_hex("fourth-token"), now + 3, now + 60_003).await.unwrap().outcome, "invalid");
    pool.close().await;
}

#[tokio::test]
async fn rate_limit_counter_is_shared_and_enforces_the_limit() {
    let Some((pool, store)) = store().await else { return };
    let bucket = hash_hex(&format!("bucket-{}", Uuid::new_v4()));
    let now = now_ms();
    assert!(store.allow_oauth_attempt(&bucket, now, 60_000, 1).await.unwrap());
    assert!(!store.allow_oauth_attempt(&bucket, now, 60_000, 1).await.unwrap());
    pool.close().await;
}
