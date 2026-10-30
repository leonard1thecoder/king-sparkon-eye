//! Tests for `POST /v1/auth/login` (issue #133) against a mock PostgREST server.
//!
//! The mock implements only the RPCs the login path calls, so the handler, rate limits, hashing
//! and token issuance all run for real over HTTP. No database is needed.

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
use king_sparkon_eye::auth::hash_password;
use king_sparkon_eye::config::Config;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tower::ServiceExt;

const OPERATOR_EMAIL: &str = "operator@example.test";
const DISABLED_EMAIL: &str = "disabled@example.test";
const PASSWORD: &str = "correct-horse-battery-1";
const SALT: &str = "0123456789abcdef";
const JWT_SECRET: &str = "login-test-signing-secret-at-least-32-bytes";
const KID: &str = "key-2026-v1";

const BCRYPT_EMAIL: &str = "bcrypt@example.test";

#[derive(Clone)]
struct MockDb {
    /// Whether the rate-limit RPCs allow the attempt.
    allow: bool,
    /// Whether the compare-and-set rehash RPC reports failure.
    fail_update: bool,
    /// Bodies of every `eye_update_password_hash` call, for assertions.
    updates: Arc<Mutex<Vec<Value>>>,
}

struct Mock {
    base: String,
    updates: Arc<Mutex<Vec<Value>>>,
}

async fn rpc(
    Path(name): Path<String>,
    State(db): State<MockDb>,
    Json(body): Json<Value>,
) -> Json<Value> {
    let hash = hash_password(PASSWORD, SALT);
    match name.as_str() {
        "eye_password_credentials_by_email" => {
            let email = body["p_email"].as_str().unwrap_or_default();
            let active = email == OPERATOR_EMAIL || email == BCRYPT_EMAIL;
            if email != OPERATOR_EMAIL && email != DISABLED_EMAIL && email != BCRYPT_EMAIL {
                return Json(json!([]));
            }
            // BCRYPT_EMAIL stores a bcrypt hash; the others store the legacy SHA-256 hash.
            let (stored_hash, salt) = if email == BCRYPT_EMAIL {
                (bcrypt::hash(PASSWORD, 4).unwrap(), String::new())
            } else {
                (hash, SALT.to_owned())
            };
            Json(json!([{
                "user_id": "usr_login_test_01",
                "email": email,
                "password_hash": stored_hash,
                "salt": salt,
                "role": "GateOperator",
                "scopes": ["enrollment:write"],
                "is_active": active,
                "created_at_ms": 1
            }]))
        }
        "eye_allow_oauth_attempt" => Json(json!([{"allowed": db.allow}])),
        "eye_update_password_hash" => {
            db.updates.lock().unwrap().push(body);
            Json(json!([{"ok": !db.fail_update}]))
        }
        "eye_issue_refresh_token" => Json(json!([{"ok": true}])),
        _ => Json(json!([])),
    }
}

async fn start_mock(allow: bool, fail_update: bool) -> Mock {
    let updates = Arc::new(Mutex::new(Vec::new()));
    let app = Router::new()
        .route("/rest/v1/rpc/{name}", post(rpc))
        .with_state(MockDb {
            allow,
            fail_update,
            updates: updates.clone(),
        });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Mock {
        base: format!("http://{addr}/rest/v1"),
        updates,
    }
}

fn config(database_url: &str) -> Config {
    let vars = [
        ("EYE_GOOGLE_CLIENT_ID", "test-client-id"),
        ("EYE_GOOGLE_CLIENT_SECRET", "test-client-secret"),
        (
            "EYE_GOOGLE_REDIRECT_URL",
            "http://127.0.0.1:9/v1/auth/google/callback",
        ),
        ("EYE_DATABASE_REST_URL", database_url),
        ("EYE_DATABASE_SERVICE_ROLE_KEY", "test-service-role-key"),
        (
            "EYE_OAUTH_CALLBACK_URLS",
            "http://127.0.0.1:9/v1/auth/google/callback",
        ),
        (
            "EYE_OAUTH_EXCHANGE_SECRET",
            "test-exchange-secret-at-least-32-bytes",
        ),
        ("EYE_JWT_SIGNING_SECRET", JWT_SECRET),
        ("EYE_JWT_KID", KID),
        ("EYE_OAUTH_ALLOW_LOCALHOST_HTTP", "true"),
    ];
    let owned: Vec<(String, String)> = vars
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Config::from_lookup(move |name| {
        owned
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    })
    .expect("config parses")
}

fn app(database_url: &str) -> Router {
    api::router(config(database_url))
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))))
}

async fn login(database_url: &str, email: &str, password: &str) -> (StatusCode, Value) {
    let body = json!({"email": email, "password": password}).to_string();
    let response = app(database_url)
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(body))
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
async fn correct_password_returns_a_token_the_photo_route_accepts() {
    let db = start_mock(true, false).await.base;
    let (status, body) = login(&db, OPERATOR_EMAIL, PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let token = body["accessToken"]
        .as_str()
        .expect("access token")
        .to_owned();
    assert!(body["refreshToken"].as_str().is_some());

    // Same signing key and claims as the photo route, so the token must be accepted there.
    let png = {
        let image = image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            320,
            240,
            image::Rgb([128, 128, 128]),
        ));
        let mut cursor = std::io::Cursor::new(Vec::new());
        image
            .write_to(&mut cursor, image::ImageFormat::Png)
            .unwrap();
        cursor.into_inner()
    };
    let mut multipart = Vec::new();
    multipart.extend_from_slice(b"--eye-boundary\r\nContent-Disposition: form-data; name=\"image\"; filename=\"p.png\"\r\nContent-Type: image/png\r\n\r\n");
    multipart.extend_from_slice(&png);
    multipart.extend_from_slice(b"\r\n--eye-boundary--\r\n");
    let response = api::router(config(&db))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/enrollment/photo-assessments")
                .header("content-type", "multipart/form-data; boundary=eye-boundary")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(multipart))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn unknown_email_wrong_password_and_disabled_account_look_identical() {
    let db = start_mock(true, false).await.base;
    let unknown = login(&db, "nobody@example.test", PASSWORD).await;
    let wrong = login(&db, OPERATOR_EMAIL, "not-the-password-1").await;
    let disabled = login(&db, DISABLED_EMAIL, PASSWORD).await;
    for (status, body) in [&unknown, &wrong, &disabled] {
        assert_eq!(*status, StatusCode::UNAUTHORIZED);
        assert_eq!(body["code"], "INVALID_CREDENTIALS");
    }
    assert_eq!(unknown.1, wrong.1);
    assert_eq!(wrong.1, disabled.1);
}

#[tokio::test]
async fn rate_limited_attempts_return_429_without_checking_the_password() {
    let db = start_mock(false, false).await.base;
    let (status, body) = login(&db, OPERATOR_EMAIL, PASSWORD).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(body["code"], "RATE_LIMITED");
}

#[tokio::test]
async fn malformed_credentials_are_rejected_before_lookup() {
    let db = start_mock(true, false).await.base;
    let (status, body) = login(&db, OPERATOR_EMAIL, "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "VALIDATION_FAILED");

    let (status, _) = login(&db, "not-an-email", PASSWORD).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let oversized = "p".repeat(1025);
    let (status, _) = login(&db, OPERATOR_EMAIL, &oversized).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn login_is_unavailable_without_oauth_configuration() {
    let response = api::router(Config::from_lookup(|_| None).unwrap())
        .layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 40_000))))
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"email": OPERATOR_EMAIL, "password": PASSWORD}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn bcrypt_user_logs_in_and_is_not_rewritten() {
    let mock = start_mock(true, false).await;
    let (status, body) = login(&mock.base, BCRYPT_EMAIL, PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(mock.updates.lock().unwrap().is_empty());
}

#[tokio::test]
async fn legacy_user_is_rehashed_to_bcrypt_on_successful_login() {
    let mock = start_mock(true, false).await;
    let (status, body) = login(&mock.base, OPERATOR_EMAIL, PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let updates = mock.updates.lock().unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0]["p_expected_hash"], hash_password(PASSWORD, SALT));
    let new_hash = updates[0]["p_new_hash"].as_str().unwrap();
    assert!(new_hash.starts_with("$2b$12$"), "{new_hash}");
    assert!(bcrypt::verify(PASSWORD, new_hash).unwrap());
}

#[tokio::test]
async fn failed_rehash_does_not_block_the_login() {
    let mock = start_mock(true, true).await;
    let (status, body) = login(&mock.base, OPERATOR_EMAIL, PASSWORD).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["accessToken"].as_str().is_some());
    assert_eq!(mock.updates.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn wrong_password_for_bcrypt_user_matches_unknown_account_response() {
    let mock = start_mock(true, false).await;
    let wrong = login(&mock.base, BCRYPT_EMAIL, "not-the-password-1").await;
    let unknown = login(&mock.base, "nobody@example.test", "not-the-password-1").await;
    assert_eq!(wrong.0, StatusCode::UNAUTHORIZED);
    assert_eq!(wrong, unknown);
    assert!(mock.updates.lock().unwrap().is_empty());
}
