//! End-to-end HTTP authorization tests for durable API credentials (#184).
//! Requires EYE_TEST_DATABASE_URL pointing at the dedicated Eye PostgreSQL test database.

use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    response::Response,
    Router,
};
use king_sparkon_eye::{
    auth::{sign_access_token, AccessTokenClaims, UserRole},
    config::Config,
    credentials,
    persistence::{connect, migrate, tenants::create_tenant},
};
use serde_json::{json, Value};
use std::collections::HashMap;
use tower::ServiceExt;
use uuid::Uuid;

const JWT_SECRET: &str = "jjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjj";
const JWT_KID: &str = "credential-http-test-kid";

fn config(database_url: String) -> Config {
    let values = HashMap::from([
        ("EYE_GOOGLE_CLIENT_ID", "client-id".to_owned()),
        ("EYE_GOOGLE_CLIENT_SECRET", "client-secret".to_owned()),
        (
            "EYE_GOOGLE_REDIRECT_URL",
            "https://eye.example.com/v1/auth/google/callback".to_owned(),
        ),
        ("EYE_DATABASE_URL", database_url),
        (
            "EYE_OAUTH_ENCRYPTION_KEY",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_owned(),
        ),
        (
            "EYE_OAUTH_CALLBACK_URLS",
            "https://eye.example.com/auth/callback".to_owned(),
        ),
        ("EYE_OAUTH_EXCHANGE_SECRET", "e".repeat(48)),
        ("EYE_JWT_SIGNING_SECRET", JWT_SECRET.to_owned()),
        ("EYE_JWT_KID", JWT_KID.to_owned()),
    ]);
    Config::from_lookup(|name| values.get(name).cloned()).expect("valid HTTP test config")
}

async fn test_pool() -> Option<sqlx::PgPool> {
    let Ok(url) = std::env::var("EYE_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; credential HTTP tests not run");
        return None;
    };
    let pool = connect(&url).await.expect("connect test PostgreSQL");
    migrate(&pool).await.expect("apply Eye migrations");
    Some(pool)
}

async fn seed_owner(pool: &sqlx::PgPool) -> String {
    let user_id = format!("usr_{}", Uuid::new_v4().simple());
    let email = format!("credential-http-{}@example.test", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO eye_auth_users (user_id,email,password_hash,salt,role,scopes,is_active,created_at_ms) VALUES ($1,$2,'test-hash','','Admin',ARRAY[]::text[],TRUE,1)",
    )
    .bind(&user_id)
    .bind(email)
    .execute(pool)
    .await
    .expect("seed owner");
    user_id
}

fn bearer(user_id: &str) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = AccessTokenClaims {
        iss: king_sparkon_eye::auth::jwt::EYE_TOKEN_ISSUER.to_owned(),
        sub: user_id.to_owned(),
        aud: king_sparkon_eye::auth::jwt::EYE_TOKEN_AUDIENCE.to_owned(),
        exp: now + 600,
        iat: now,
        jti: format!("tok_{}", Uuid::new_v4().simple()),
        role: UserRole::Admin,
        scopes: vec![],
        event_id: None,
    };
    sign_access_token(&claims, JWT_KID, JWT_SECRET.as_bytes()).unwrap()
}

async fn send(app: &Router, request: Request<Body>) -> Response {
    app.clone().oneshot(request).await.expect("HTTP response")
}

async fn json_body(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

async fn post_json(app: &Router, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    json_body(send(app, request).await).await
}

async fn get_machine(app: &Router, secret: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri("/v1/machine/whoami")
        .header("authorization", format!("Bearer {secret}"))
        .body(Body::empty())
        .unwrap();
    json_body(send(app, request).await).await
}

#[tokio::test]
async fn http_management_rotation_machine_auth_and_cross_tenant_rejection() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let owner_a = seed_owner(&pool).await;
    let owner_b = seed_owner(&pool).await;
    let tenant_a = create_tenant(&pool, &owner_a, "HTTP tenant A", 1_800_000_000_000)
        .await
        .unwrap()
        .tenant_id;
    let tenant_b = create_tenant(&pool, &owner_b, "HTTP tenant B", 1_800_000_000_001)
        .await
        .unwrap()
        .tenant_id;
    let app = credentials::router(config(std::env::var("EYE_TEST_DATABASE_URL").unwrap()));
    let owner_a_token = bearer(&owner_a);
    let owner_b_token = bearer(&owner_b);

    // Human JWT owner creates a tenant-bound client.
    let client_uri = format!("/v1/tenants/{}/api-clients", tenant_a.as_str());
    let (status, client) = post_json(
        &app,
        &client_uri,
        &owner_a_token,
        json!({
            "displayName": "HTTP integration client",
            "scopes": ["identities:read", "verification:create"],
            "rateLimitPerMinute": 50
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let client_id = client["clientId"].as_str().unwrap().to_owned();

    // A different tenant owner cannot use their tenant selector to target this client.
    let credential_uri = format!(
        "/v1/tenants/{}/api-clients/{client_id}/credentials",
        tenant_b.as_str()
    );
    let (status, _) = post_json(
        &app,
        &credential_uri,
        &owner_b_token,
        json!({"scopes": ["identities:read"], "environment": "test"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let credential_uri = format!(
        "/v1/tenants/{}/api-clients/{client_id}/credentials",
        tenant_a.as_str()
    );
    let (status, issued) = post_json(
        &app,
        &credential_uri,
        &owner_a_token,
        json!({"scopes": ["identities:read"], "environment": "test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let old_secret = issued["secret"].as_str().unwrap().to_owned();
    let old_id = issued["credentialId"].as_str().unwrap().to_owned();

    // Machine identity is derived from the credential row, never request tenant data.
    let (status, identity) = get_machine(&app, &old_secret).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(identity["tenantId"], tenant_a.as_str());
    assert_eq!(identity["ownerUserId"], owner_a);

    // A different human owner cannot rotate this credential.
    let rotate_uri = format!(
        "/v1/tenants/{}/api-credentials/{old_id}/rotate",
        tenant_a.as_str()
    );
    let (status, _) = post_json(
        &app,
        &rotate_uri,
        &owner_b_token,
        json!({"environment": "test"}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Rotation is atomic: old key is invalid, replacement key is immediately usable.
    let (status, rotated) = post_json(
        &app,
        &rotate_uri,
        &owner_a_token,
        json!({"environment": "test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let new_secret = rotated["secret"].as_str().unwrap().to_owned();

    let (status, _) = get_machine(&app, &old_secret).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, identity) = get_machine(&app, &new_secret).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(identity["tenantId"], tenant_a.as_str());

    // Credential lacking identities:read is denied at the actual HTTP authorization layer.
    let (status, restricted) = post_json(
        &app,
        &credential_uri,
        &owner_a_token,
        json!({"scopes": ["verification:create"], "environment": "test"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let restricted_secret = restricted["secret"].as_str().unwrap();
    let (status, _) = get_machine(&app, restricted_secret).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
