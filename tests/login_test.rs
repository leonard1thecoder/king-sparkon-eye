//! Password login route tests against the SQLx-backed Eye PostgreSQL store.
//!
//! Requires EYE_TEST_DATABASE_URL pointing to a disposable PostgreSQL database.
//! Tests skip explicitly when it is absent; skipped tests are not DB verification.

use std::collections::HashMap;
use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use http_body_util::BodyExt;
use king_sparkon_eye::api;
use king_sparkon_eye::auth::hash_password as legacy_hash_password;
use king_sparkon_eye::config::Config;
use king_sparkon_eye::persistence::{connect, migrate};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use uuid::Uuid;

const PASSWORD: &str = "correct-horse-battery-1";
const SALT: &str = "0123456789abcdef";
const JWT_SECRET: &str = "login-test-signing-secret-at-least-32-bytes";
const KID: &str = "key-2026-v1";
const REDIRECT_URL: &str = "http://127.0.0.1:9/v1/auth/google/callback";
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
            eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; password login DB tests not run");
            return None;
        }
    };
    assert_disposable_test_database_url(&url);
    let pool = connect(&url)
        .await
        .expect("connect to disposable PostgreSQL database");
    migrate(&pool).await.expect("apply Eye SQLx migrations");
    sqlx::query("DELETE FROM eye_oauth_rate_limits")
        .execute(&pool)
        .await
        .expect("reset test rate limits");
    Some((pool, url))
}

fn config(database_url: &str) -> Config {
    let values = HashMap::from([
        ("EYE_GOOGLE_CLIENT_ID", "test-client-id".to_owned()),
        ("EYE_GOOGLE_CLIENT_SECRET", "test-client-secret".to_owned()),
        ("EYE_GOOGLE_REDIRECT_URL", REDIRECT_URL.to_owned()),
        ("EYE_DATABASE_URL", database_url.to_owned()),
        ("EYE_OAUTH_ENCRYPTION_KEY", ENCRYPTION_KEY.to_owned()),
        ("EYE_OAUTH_CALLBACK_URLS", REDIRECT_URL.to_owned()),
        (
            "EYE_OAUTH_EXCHANGE_SECRET",
            "test-exchange-secret-at-least-32-bytes".to_owned(),
        ),
        ("EYE_JWT_SIGNING_SECRET", JWT_SECRET.to_owned()),
        ("EYE_JWT_KID", KID.to_owned()),
        ("EYE_OAUTH_ALLOW_LOCALHOST_HTTP", "true".to_owned()),
    ]);
    Config::from_lookup(|name| values.get(name).cloned()).expect("valid SQLx OAuth config")
}

async fn seed_user(
    pool: &PgPool,
    email: &str,
    password_hash: &str,
    salt: &str,
    active: bool,
) -> String {
    let user_id = format!("usr_{}", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO eye_auth_users(user_id,email,password_hash,salt,role,scopes,is_active,created_at_ms)
         VALUES($1,$2,$3,$4,'GateOperator','{}', $5, 1)",
    )
    .bind(&user_id)
    .bind(email)
    .bind(password_hash)
    .bind(salt)
    .bind(active)
    .execute(pool)
    .await
    .expect("seed password user");
    user_id
}

async fn login(database_url: &str, email: &str, password: &str) -> (axum::http::StatusCode, Value) {
    let app = api::router(config(database_url))
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))));
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"email": email, "password": password}).to_string(),
                ))
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
async fn legacy_password_login_returns_tokens_and_rehashes_in_postgres() {
    let Some((pool, url)) = database().await else {
        return;
    };
    let email = format!("legacy-{}@example.test", Uuid::new_v4());
    let user_id = seed_user(
        &pool,
        &email,
        &legacy_hash_password(PASSWORD, SALT),
        SALT,
        true,
    )
    .await;

    let (status, body) = login(&url, &email, PASSWORD).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    assert!(body["accessToken"].as_str().is_some());
    assert!(body["refreshToken"].as_str().is_some());

    let stored: String =
        sqlx::query_scalar("SELECT password_hash FROM eye_auth_users WHERE user_id=$1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(stored.starts_with("$2b$12$"), "{stored}");
    assert!(bcrypt::verify(PASSWORD, &stored).unwrap());
    pool.close().await;
}

#[tokio::test]
async fn bcrypt_login_does_not_rewrite_the_existing_hash() {
    let Some((pool, url)) = database().await else {
        return;
    };
    let email = format!("bcrypt-{}@example.test", Uuid::new_v4());
    let stored_hash = bcrypt::hash(PASSWORD, 4).unwrap();
    let user_id = seed_user(&pool, &email, &stored_hash, "", true).await;

    let (status, body) = login(&url, &email, PASSWORD).await;
    assert_eq!(status, axum::http::StatusCode::OK, "{body}");
    let actual: String =
        sqlx::query_scalar("SELECT password_hash FROM eye_auth_users WHERE user_id=$1")
            .bind(user_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(actual, stored_hash);
    pool.close().await;
}

#[tokio::test]
async fn unknown_wrong_password_and_disabled_account_have_uniform_errors() {
    let Some((pool, url)) = database().await else {
        return;
    };
    let email = format!("operator-{}@example.test", Uuid::new_v4());
    let disabled_email = format!("disabled-{}@example.test", Uuid::new_v4());
    seed_user(
        &pool,
        &email,
        &legacy_hash_password(PASSWORD, SALT),
        SALT,
        true,
    )
    .await;
    seed_user(
        &pool,
        &disabled_email,
        &legacy_hash_password(PASSWORD, SALT),
        SALT,
        false,
    )
    .await;

    let unknown = login(
        &url,
        &format!("missing-{}@example.test", Uuid::new_v4()),
        PASSWORD,
    )
    .await;
    let wrong = login(&url, &email, "not-the-password-1").await;
    let disabled = login(&url, &disabled_email, PASSWORD).await;
    for (status, body) in [&unknown, &wrong, &disabled] {
        assert_eq!(*status, axum::http::StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "INVALID_CREDENTIALS");
    }
    assert_eq!(unknown.1, wrong.1);
    assert_eq!(wrong.1, disabled.1);
    pool.close().await;
}

#[tokio::test]
async fn malformed_credentials_are_rejected_before_database_lookup() {
    let Some((pool, url)) = database().await else {
        return;
    };
    let (status, body) = login(&url, "operator@example.test", "").await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "VALIDATION_FAILED");

    let (status, _) = login(&url, "not-an-email", PASSWORD).await;
    assert_eq!(status, axum::http::StatusCode::BAD_REQUEST);
    pool.close().await;
}

#[tokio::test]
async fn login_is_unavailable_without_oauth_configuration() {
    let response = api::router(Config::from_lookup(|_| None).unwrap())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))))
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"email": "operator@example.test", "password": PASSWORD}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        axum::http::StatusCode::SERVICE_UNAVAILABLE
    );
}
