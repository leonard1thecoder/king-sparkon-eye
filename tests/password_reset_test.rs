//! Tests for `POST /v1/auth/password-reset/request` and `POST /v1/auth/password-reset/confirm`
//! against a mock PostgREST server.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::routing::post;
use http_body_util::BodyExt;
use king_sparkon_eye::api;
use king_sparkon_eye::config::Config;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower::ServiceExt;

const EXISTING_EMAIL: &str = "existing@example.test";
const VALID_TOKEN: &str = "test_valid_reset_token_123456789";
const NEW_PASSWORD: &str = "brand-new-secure-password-2026";
const JWT_SECRET: &str = "login-test-signing-secret-at-least-32-bytes";
const KID: &str = "key-2026-v1";

#[derive(Clone)]
struct MockDb {
    created_tokens: Arc<Mutex<Vec<String>>>,
}

struct Mock {
    base: String,
}

async fn rpc(
    Path(name): Path<String>,
    State(db): State<MockDb>,
    Json(body): Json<Value>,
) -> Json<Value> {
    match name.as_str() {
        "eye_allow_oauth_attempt" => Json(json!([{"ok": true}])),
        "eye_create_password_reset_token" => {
            let email = body["p_email"].as_str().unwrap_or_default();
            if email == EXISTING_EMAIL {
                db.created_tokens.lock().unwrap().push(body["p_token_hash"].as_str().unwrap().to_string());
                Json(json!([{
                    "user_id": "usr_existing_123",
                    "email": email,
                }]))
            } else {
                Json(json!([]))
            }
        }
        "eye_consume_password_reset_token" => {
            let token_hash = body["p_token_hash"].as_str().unwrap_or_default();
            if token_hash.contains("valid") || db.created_tokens.lock().unwrap().iter().any(|t| t == token_hash) {
                Json(json!([{"ok": true}]))
            } else {
                Json(json!([{"ok": false}]))
            }
        }
        _ => Json(json!([])),
    }
}

async fn spawn_mock() -> Mock {
    let db = MockDb {
        created_tokens: Arc::new(Mutex::new(Vec::new())),
    };
    let app = Router::new()
        .route("/rpc/{name}", post(rpc))
        .with_state(db);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Mock {
        base: format!("http://{addr}"),
    }
}

fn app_with(mock: &Mock) -> Router {
    let config = Config::from_lookup(|key| match key {
        "EYE_JWT_HMAC_SECRET" => Some(JWT_SECRET.to_owned()),
        "EYE_JWT_ACTIVE_KID" => Some(KID.to_owned()),
        "EYE_POSTGREST_BASE_URL" => Some(mock.base.clone()),
        "EYE_POSTGREST_SERVICE_KEY" => Some("secret".to_owned()),
        "EYE_COOKIE_SECRET" => Some("0123456789abcdef0123456789abcdef0123456789abcdef".to_owned()),
        "EYE_GOOGLE_CLIENT_ID" => Some("client-id".to_owned()),
        "EYE_GOOGLE_REDIRECT_URL" => Some("https://eye.test/v1/auth/google/callback".to_owned()),
        "EYE_GOOGLE_PKCE_ENCRYPTION_KEY" => {
            Some("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned())
        }
        "EYE_OAUTH_EXCHANGE_SECRET" => Some("exchange-secret".to_owned()),
        _ => None,
    })
    .expect("config is valid");
    api::router(config)
}

#[tokio::test]
async fn request_password_reset_returns_ok_and_prevents_enumeration() {
    let mock = spawn_mock().await;
    let app = app_with(&mock);

    let peer: SocketAddr = "192.0.2.10:1234".parse().unwrap();

    // 1. Existing user
    let body = serde_json::to_vec(&json!({
        "email": EXISTING_EMAIL,
    }))
    .unwrap();
    let req = Request::builder()
        .uri("/v1/auth/password-reset/request")
        .method("POST")
        .header("content-type", "application/json")
        .extension(MockConnectInfo(peer))
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // 2. Non-existent user also returns OK (anti-enumeration)
    let body = serde_json::to_vec(&json!({
        "email": "unknown@example.test",
    }))
    .unwrap();
    let req = Request::builder()
        .uri("/v1/auth/password-reset/request")
        .method("POST")
        .header("content-type", "application/json")
        .extension(MockConnectInfo(peer))
        .body(Body::from(body))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn confirm_password_reset_with_valid_token_succeeds() {
    let mock = spawn_mock().await;
    let app = app_with(&mock);

    let peer: SocketAddr = "192.0.2.11:1234".parse().unwrap();

    let body = serde_json::to_vec(&json!({
        "token": VALID_TOKEN,
        "password": NEW_PASSWORD,
    }))
    .unwrap();
    let req = Request::builder()
        .uri("/v1/auth/password-reset/confirm")
        .method("POST")
        .header("content-type", "application/json")
        .extension(MockConnectInfo(peer))
        .body(Body::from(body))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn confirm_password_reset_with_invalid_or_expired_token_fails() {
    let mock = spawn_mock().await;
    let app = app_with(&mock);

    let peer: SocketAddr = "192.0.2.12:1234".parse().unwrap();

    let body = serde_json::to_vec(&json!({
        "token": "expired_or_invalid_token",
        "password": NEW_PASSWORD,
    }))
    .unwrap();
    let req = Request::builder()
        .uri("/v1/auth/password-reset/confirm")
        .method("POST")
        .header("content-type", "application/json")
        .extension(MockConnectInfo(peer))
        .body(Body::from(body))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let err: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(err["code"], "INVALID_OR_EXPIRED_TOKEN");
}
