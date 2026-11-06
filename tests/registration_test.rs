//! Registration route tests against Eye-owned SQLx PostgreSQL persistence.
//!
//! Requires EYE_TEST_DATABASE_URL pointing to a disposable PostgreSQL database.
//! Tests skip explicitly when it is absent; skipped tests are not DB verification.

use std::{collections::HashMap, net::SocketAddr};

use axum::{body::Body, extract::connect_info::MockConnectInfo, http::Request};
use base64::Engine;
use http_body_util::BodyExt;
use king_sparkon_eye::{
    api,
    config::Config,
    persistence::{connect, migrate},
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct-horse-battery-1";
const ENCRYPTION_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";

async fn database() -> Option<(PgPool, String)> {
    let url = match std::env::var("EYE_TEST_DATABASE_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; registration DB tests not run");
            return None;
        }
    };
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

async fn seed_user(pool: &PgPool, email: &str, scopes: &[&str]) -> String {
    let user_id = format!("usr_{}", Uuid::new_v4().simple());
    let scopes = scopes
        .iter()
        .map(|scope| (*scope).to_owned())
        .collect::<Vec<_>>();
    sqlx::query(
        "INSERT INTO eye_auth_users(user_id,email,password_hash,salt,role,scopes,is_active,created_at_ms)
         VALUES($1,lower($2),'seeded-password-hash','seeded-salt','GateOperator',$3,true,1)",
    )
    .bind(&user_id)
    .bind(email)
    .bind(scopes)
    .execute(pool)
    .await
    .expect("seed registered account");
    user_id
}

async fn post(database_url: &str, path: &str, payload: Value) -> (axum::http::StatusCode, Value) {
    let app = api::router(config(database_url))
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 42000))));
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
async fn registration_persists_account_without_enrollment_scope() {
    let Some((pool, url)) = database().await else {
        return;
    };
    let email = format!("new-{}@example.test", Uuid::new_v4());
    let (status, body) = post(
        &url,
        "/v1/auth/register",
        json!({"email": email, "password": PASSWORD}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CREATED, "{body}");

    let (role, scopes, active): (String, Vec<String>, bool) = sqlx::query_as(
        "SELECT role,scopes,is_active FROM eye_auth_users WHERE lower(email)=lower($1)",
    )
    .bind(&email)
    .fetch_one(&pool)
    .await
    .expect("read registered account");
    assert_eq!(role, "GateOperator");
    assert!(scopes.is_empty());
    assert!(active);

    let (login_status, login_body) = post(
        &url,
        "/v1/auth/login",
        json!({"email": email, "password": PASSWORD}),
    )
    .await;
    assert_eq!(login_status, axum::http::StatusCode::OK, "{login_body}");
    let token = login_body["accessToken"].as_str().expect("access token");
    let payload = token.split('.').nth(1).expect("JWT payload");
    let claims_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .expect("base64url JWT payload");
    let claims: Value = serde_json::from_slice(&claims_bytes).unwrap();
    assert_eq!(claims["scopes"], json!([]));
    assert!(
        !claims["scopes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|scope| scope == "enrollment:write")
    );
    pool.close().await;
}

#[tokio::test]
async fn registering_existing_email_returns_conflict() {
    let Some((pool, url)) = database().await else {
        return;
    };
    let email = format!("existing-{}@example.test", Uuid::new_v4());
    seed_user(&pool, &email, &["enrollment:write"]).await;

    let (status, body) = post(
        &url,
        "/v1/auth/register",
        json!({"email": email, "password": PASSWORD}),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "USER_ALREADY_EXISTS");
    pool.close().await;
}

#[tokio::test]
async fn invalid_email_or_weak_password_returns_validation_error() {
    let Some((pool, url)) = database().await else {
        return;
    };
    let weak = post(
        &url,
        "/v1/auth/register",
        json!({"email": "valid@example.test", "password": "short"}),
    )
    .await;
    assert_eq!(weak.0, axum::http::StatusCode::BAD_REQUEST);

    let invalid_email = post(
        &url,
        "/v1/auth/register",
        json!({"email": "notanemail", "password": "validpassword123"}),
    )
    .await;
    assert_eq!(invalid_email.0, axum::http::StatusCode::BAD_REQUEST);
    pool.close().await;
}
