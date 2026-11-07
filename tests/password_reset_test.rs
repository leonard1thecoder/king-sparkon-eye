//! Password reset route tests against the Eye-owned SQLx PostgreSQL store.
//!
//! Requires EYE_TEST_DATABASE_URL pointing to a disposable PostgreSQL database.
//! Tests skip explicitly when it is absent; skipped tests are not DB verification.

use std::{collections::HashMap, net::SocketAddr};

use axum::{body::Body, extract::connect_info::MockConnectInfo, http::Request};
use http_body_util::BodyExt;
use king_sparkon_eye::{
    api,
    auth::durable_store::hash_hex,
    config::Config,
    persistence::{connect, migrate},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const NEW_PASSWORD: &str = "brand-new-secure-password-2026";
const ENCRYPTION_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";


fn assert_disposable_test_database_url(url: &str) {
    let parsed = reqwest::Url::parse(url).expect("EYE_TEST_DATABASE_URL must be a valid PostgreSQL URL");
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let username = parsed.username().to_ascii_lowercase();
    let database_name = parsed.path().trim_matches('/').to_ascii_lowercase();
    let loopback = matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]");

    if loopback {
        assert!(database_name.contains("test"), "test database name must contain 'test'");
        return;
    }

    assert_eq!(
        std::env::var("EYE_TEST_DATABASE_ALLOW_SUPABASE").as_deref(),
        Ok("1"),
        "remote PostgreSQL is blocked; explicitly opt in only for a dedicated Supabase test project"
    );
    let direct_project_ref = host
        .strip_prefix("db.")
        .and_then(|value| value.strip_suffix(".supabase.co"));
    let pooler_project_ref = if host.ends_with(".pooler.supabase.com") {
        username.strip_prefix("postgres.")
    } else {
        None
    };
    assert!(
        direct_project_ref.is_some_and(|project_ref| !project_ref.is_empty() && username == "postgres")
            || pooler_project_ref.is_some_and(|project_ref| !project_ref.is_empty()),
        "remote test URL must be a Supabase direct host (db.<project-ref>.supabase.co) or pooler host with postgres.<project-ref> user"
    );
}

async fn database() -> Option<(PgPool, String)> {
    let url = match std::env::var("EYE_TEST_DATABASE_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; password reset DB tests not run");
            return None;
        }
    };
    assert_disposable_test_database_url(&url);
    let pool = connect(&url)
        .await
        .expect("connect to disposable PostgreSQL database");
    migrate(&pool).await.expect("apply Eye SQLx migrations");
    Some((pool, url))
}

fn config(database_url: &str) -> Config {
    let values = HashMap::from([
        ("EYE_GOOGLE_CLIENT_ID", "test-client-id".to_owned()),
        ("EYE_GOOGLE_CLIENT_SECRET", "test-client-secret".to_owned()),
        (
            "EYE_GOOGLE_REDIRECT_URL",
            "http://127.0.0.1:9/v1/auth/google/callback".to_owned(),
        ),
        ("EYE_DATABASE_URL", database_url.to_owned()),
        ("EYE_OAUTH_ENCRYPTION_KEY", ENCRYPTION_KEY.to_owned()),
        (
            "EYE_OAUTH_CALLBACK_URLS",
            "http://127.0.0.1:9/v1/auth/google/callback".to_owned(),
        ),
        (
            "EYE_OAUTH_EXCHANGE_SECRET",
            "test-exchange-secret-at-least-32-bytes".to_owned(),
        ),
        (
            "EYE_JWT_SIGNING_SECRET",
            "test-jwt-signing-secret-at-least-32-bytes".to_owned(),
        ),
        ("EYE_OAUTH_ALLOW_LOCALHOST_HTTP", "true".to_owned()),
    ]);
    Config::from_lookup(|name| values.get(name).cloned()).expect("valid SQLx auth config")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as i64
}

async fn seed_user(pool: &PgPool, email: &str) -> String {
    let user_id = format!("usr_{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO eye_auth_users(user_id,email,password_hash,salt,role,scopes,is_active,created_at_ms)
         VALUES($1,lower($2),'old-password-hash','old-salt','GateOperator','{}',true,$3)",
    )
    .bind(&user_id)
    .bind(email)
    .bind(now_ms())
    .execute(pool)
    .await
    .expect("seed password-reset user");
    user_id
}

async fn post(database_url: &str, path: &str, payload: Value) -> (axum::http::StatusCode, Value) {
    let app = api::router(config(database_url))
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 41000))));
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

#[tokio::test]
async fn password_reset_request_prevents_account_enumeration() {
    let Some((pool, url)) = database().await else {
        return;
    };
    let email = format!("existing-{}@example.test", Uuid::new_v4());
    seed_user(&pool, &email).await;

    let existing = post(
        &url,
        "/v1/auth/password-reset/request",
        json!({"email": email}),
    )
    .await;
    let unknown = post(
        &url,
        "/v1/auth/password-reset/request",
        json!({"email": format!("missing-{}@example.test", Uuid::new_v4())}),
    )
    .await;
    assert_eq!(existing.0, axum::http::StatusCode::OK, "{:?}", existing.1);
    assert_eq!(unknown.0, axum::http::StatusCode::OK, "{:?}", unknown.1);
    assert_eq!(existing.1, unknown.1);
    pool.close().await;
}

#[tokio::test]
async fn password_reset_confirm_consumes_token_and_updates_password() {
    let Some((pool, url)) = database().await else {
        return;
    };
    let email = format!("reset-{}@example.test", Uuid::new_v4());
    let user_id = seed_user(&pool, &email).await;
    let token = format!("reset-token-{}", Uuid::new_v4());
    let token_hash = hash_hex(&token);
    let now = now_ms();
    sqlx::query(
        "INSERT INTO eye_password_reset_tokens(token_hash,user_id,created_at_ms,expires_at_ms)
         VALUES(decode($1,'hex'),$2,$3,$4)",
    )
    .bind(&token_hash)
    .bind(&user_id)
    .bind(now)
    .bind(now + 60_000)
    .execute(&pool)
    .await
    .expect("seed one-time reset token");

    let result = post(
        &url,
        "/v1/auth/password-reset/confirm",
        json!({"token": token, "password": NEW_PASSWORD}),
    )
    .await;
    assert_eq!(result.0, axum::http::StatusCode::OK, "{:?}", result.1);

    let consumed: Option<i64> = sqlx::query_scalar(
        "SELECT consumed_at_ms FROM eye_password_reset_tokens WHERE token_hash=decode($1,'hex')",
    )
    .bind(&token_hash)
    .fetch_one(&pool)
    .await
    .expect("read reset token");
    assert!(consumed.is_some());

    let password_hash: String =
        sqlx::query_scalar("SELECT password_hash FROM eye_auth_users WHERE user_id=$1")
            .bind(&user_id)
            .fetch_one(&pool)
            .await
            .expect("read updated password hash");
    assert_ne!(password_hash, "old-password-hash");
    pool.close().await;
}

#[tokio::test]
async fn invalid_or_expired_password_reset_token_is_rejected() {
    let Some((pool, url)) = database().await else {
        return;
    };
    let result = post(
        &url,
        "/v1/auth/password-reset/confirm",
        json!({"token": format!("invalid-{}", Uuid::new_v4()), "password": NEW_PASSWORD}),
    )
    .await;
    assert_eq!(result.0, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(result.1["code"], "INVALID_OR_EXPIRED_TOKEN");
    pool.close().await;
}
