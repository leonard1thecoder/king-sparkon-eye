//! Tests for `POST /v1/auth/register` against a mock PostgREST server.

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

const OPERATOR_EMAIL: &str = "operator@example.test";
const NEW_EMAIL: &str = "newuser@example.test";
const PASSWORD: &str = "correct-horse-battery-1";
const JWT_SECRET: &str = "login-test-signing-secret-at-least-32-bytes";
const KID: &str = "key-2026-v1";

#[derive(Clone)]
struct MockDb {
    registered_users: Arc<Mutex<Vec<String>>>,
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
        "eye_password_credentials_by_email" => {
            let email = body["p_email"].as_str().unwrap_or_default();
            if email == OPERATOR_EMAIL || db.registered_users.lock().unwrap().contains(&email.to_string()) {
                Json(json!([{
                    "user_id": "usr_existing",
                    "email": email,
                    "password_hash": "$2b$12$e8Y7X4Q9w7",
                    "salt": "somesalt",
                    "role": "GateOperator",
                    "scopes": ["enrollment:write"],
                    "is_active": true,
                    "created_at_ms": 1_700_000_000_000u64,
                }]))
            } else {
                Json(json!([]))
            }
        }
        "eye_register_user" => {
            let email = body["p_email"].as_str().unwrap_or_default();
            let mut users = db.registered_users.lock().unwrap();
            if email == OPERATOR_EMAIL || users.contains(&email.to_string()) {
                Json(json!([]))
            } else {
                users.push(email.to_string());
                Json(json!([{
                    "user_id": body["p_user_id"],
                    "email": email,
                    "role": body["p_role"],
                    "scopes": body["p_scopes"],
                    "is_active": true,
                    "created_at_ms": body["p_now_ms"],
                }]))
            }
        }
        _ => Json(json!([])),
    }
}

async fn spawn_mock() -> Mock {
    let db = MockDb {
        registered_users: Arc::new(Mutex::new(Vec::new())),
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
async fn valid_registration_succeeds_with_created_status() {
    let mock = spawn_mock().await;
    let app = app_with(&mock);

    let body = serde_json::to_vec(&json!({
        "email": NEW_EMAIL,
        "password": PASSWORD,
    }))
    .unwrap();

    let peer: SocketAddr = "192.0.2.1:1234".parse().unwrap();
    let req = Request::builder()
        .uri("/v1/auth/register")
        .method("POST")
        .header("content-type", "application/json")
        .extension(MockConnectInfo(peer))
        .body(Body::from(body))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn registering_existing_email_returns_conflict() {
    let mock = spawn_mock().await;
    let app = app_with(&mock);

    let body = serde_json::to_vec(&json!({
        "email": OPERATOR_EMAIL,
        "password": PASSWORD,
    }))
    .unwrap();

    let peer: SocketAddr = "192.0.2.2:1234".parse().unwrap();
    let req = Request::builder()
        .uri("/v1/auth/register")
        .method("POST")
        .header("content-type", "application/json")
        .extension(MockConnectInfo(peer))
        .body(Body::from(body))
        .unwrap();

    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let err: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(err["code"], "USER_ALREADY_EXISTS");
}

#[tokio::test]
async fn invalid_email_or_weak_password_returns_validation_error() {
    let mock = spawn_mock().await;
    let app = app_with(&mock);

    let peer: SocketAddr = "192.0.2.3:1234".parse().unwrap();

    // Short password (< 10)
    let body = serde_json::to_vec(&json!({
        "email": "user@example.test",
        "password": "short",
    }))
    .unwrap();
    let req = Request::builder()
        .uri("/v1/auth/register")
        .method("POST")
        .header("content-type", "application/json")
        .extension(MockConnectInfo(peer))
        .body(Body::from(body))
        .unwrap();
    let response = app.clone().oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    // Invalid email (no @)
    let body = serde_json::to_vec(&json!({
        "email": "notanemail",
        "password": "validpassword123",
    }))
    .unwrap();
    let req = Request::builder()
        .uri("/v1/auth/register")
        .method("POST")
        .header("content-type", "application/json")
        .extension(MockConnectInfo(peer))
        .body(Body::from(body))
        .unwrap();
    let response = app.oneshot(req).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}
