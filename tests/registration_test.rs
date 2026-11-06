//! Tests for `POST /v1/auth/register` against a mock PostgREST server.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use base64::Engine;

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
    registered_users: Arc<Mutex<HashMap<String, Value>>>,
}

struct Mock {
    base: String,
    registered_users: Arc<Mutex<HashMap<String, Value>>>,
}

async fn rpc(
    Path(name): Path<String>,
    State(db): State<MockDb>,
    Json(body): Json<Value>,
) -> Json<Value> {
    match name.as_str() {
        "eye_allow_oauth_attempt" => Json(json!([{"allowed": true}])),
        "eye_password_credentials_by_email" => {
            let email = body["p_email"].as_str().unwrap_or_default();
            if email == OPERATOR_EMAIL {
                // Pre-provisioned operator: the only account seeded with enrollment:write.
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
            } else if let Some(user) = db.registered_users.lock().unwrap().get(email) {
                Json(json!([{
                    "user_id": user["p_user_id"],
                    "email": user["p_email"],
                    "password_hash": user["p_password_hash"],
                    "salt": user["p_salt"],
                    "role": user["p_role"],
                    "scopes": user["p_scopes"],
                    "is_active": true,
                    "created_at_ms": user["p_now_ms"],
                }]))
            } else {
                Json(json!([]))
            }
        }
        "eye_register_user" => {
            let email = body["p_email"].as_str().unwrap_or_default();
            let mut users = db.registered_users.lock().unwrap();
            if email == OPERATOR_EMAIL || users.contains_key(email) {
                Json(json!([]))
            } else {
                users.insert(email.to_string(), body.clone());
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
        "eye_issue_refresh_token" => Json(json!([{"ok": true}])),
        _ => Json(json!([])),
    }
}

async fn spawn_mock() -> Mock {
    let registered_users = Arc::new(Mutex::new(HashMap::new()));
    let db = MockDb {
        registered_users: registered_users.clone(),
    };
    let app = Router::new()
        .route("/rest/v1/rpc/{name}", post(rpc))
        .with_state(db);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Mock {
        base: format!("http://{addr}/rest/v1"),
        registered_users,
    }
}

fn app_with(mock: &Mock) -> Router {
    // Mirrors the settings `Config` requires for the OAuth/database runtime
    // (see tests/login_test.rs), pointed at the mock PostgREST server.
    let vars = [
        ("EYE_GOOGLE_CLIENT_ID", "test-client-id"),
        ("EYE_GOOGLE_CLIENT_SECRET", "test-client-secret"),
        (
            "EYE_GOOGLE_REDIRECT_URL",
            "http://127.0.0.1:9/v1/auth/google/callback",
        ),
        ("EYE_DATABASE_REST_URL", mock.base.as_str()),
        ("EYE_DATABASE_SERVICE_ROLE_KEY", "test-service-role-key"),
        (
            "EYE_OAUTH_CALLBACK_URLS",
            "http://127.0.0.1:9/v1/auth/google/callback",
        ),
        (
            "EYE_OAUTH_EXCHANGE_SECRET",
            "exchange-secret-at-least-32-bytes-long",
        ),
        ("EYE_JWT_SIGNING_SECRET", JWT_SECRET),
        ("EYE_JWT_KID", KID),
        ("EYE_OAUTH_ALLOW_LOCALHOST_HTTP", "true"),
    ];
    let config = Config::from_lookup(|key| {
        vars.iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| (*value).to_owned())
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
async fn public_registration_does_not_grant_enrollment_permission() {
    let mock = spawn_mock().await;
    let app = app_with(&mock);
    let peer: SocketAddr = "192.0.2.4:1234".parse().unwrap();

    let registration = serde_json::to_vec(&json!({
        "email": NEW_EMAIL,
        "password": PASSWORD,
    }))
    .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/auth/register")
                .method("POST")
                .header("content-type", "application/json")
                .extension(MockConnectInfo(peer))
                .body(Body::from(registration))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);

    // The registration RPC must carry an empty scope list: no client-supplied or
    // default `enrollment:write` may reach the database at account creation.
    {
        let users = mock.registered_users.lock().unwrap();
        let stored = users.get(NEW_EMAIL).expect("registration RPC was recorded");
        assert_eq!(stored["p_scopes"], json!([]));
        assert_eq!(stored["p_role"], json!("GateOperator"));
    }

    let login = serde_json::to_vec(&json!({
        "email": NEW_EMAIL,
        "password": PASSWORD,
    }))
    .unwrap();
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/auth/login")
                .method("POST")
                .header("content-type", "application/json")
                .extension(MockConnectInfo(peer))
                .body(Body::from(login))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    let token = body["accessToken"].as_str().expect("access token");
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
