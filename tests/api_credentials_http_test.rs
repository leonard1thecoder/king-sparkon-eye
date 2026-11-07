//! End-to-end HTTP authorization tests for durable API credentials (#184).
//! Requires EYE_TEST_DATABASE_URL pointing at the dedicated Eye PostgreSQL test database.

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use king_sparkon_eye::{
    auth::{AccessTokenClaims, UserRole, sign_access_token},
    config::Config,
    credentials,
    persistence::{connect, migrate, tenants::create_tenant},
};
use serde_json::{Value, json};
use std::collections::HashMap;
use tower::ServiceExt;
use uuid::Uuid;

const JWT_SECRET: &str = "jjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjjj";
const JWT_KID: &str = "credential-http-test-kid";

fn config(database_url: String) -> Config {
    let values = HashMap::from([
        ("EYE_GOOGLE_CLIENT_ID", "client-id".to_owned()),
        ("EYE_GOOGLE_CLIENT_SECRET", "client-secret".to_owned()),
        ("EYE_GOOGLE_REDIRECT_URL", "https://eye.example.com/v1/auth/google/callback".to_owned()),
        ("EYE_DATABASE_URL", database_url),
        ("EYE_OAUTH_ENCRYPTION_KEY", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_owned()),
        ("EYE_OAUTH_CALLBACK_URLS", "https://eye.example.com/auth/callback".to_owned()),
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

async fn send(app: &axum::Router, request: Request<Body>) -> axum::response::Response {
    app.clone().oneshot(request).await.expect("HTTP response")
}

async fn json_body(response: axum::response::Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, value)
}

#[tokio::test]
async fn http_management_rotation_machine_auth_and_cross_tenant_rejection() {
    let Some(pool) = test_pool().await else { return };
    let owner_a = seed_owner(&pool).await;
    let owner_b = seed_owner(&pool).await;
    let tenant_a = create_tenant(&pool, &owner_a, "HTTP tenant A", 1_800_000_000_000)
        .await.unwrap().tenant_id;
    let tenant_b = create_tenant(&pool, &owner_b, "HTTP tenant B", 1_800_000_000_001)
        .await.unwrap().tenant_id;
    let app = credentials::router(config(std::env::var("EYE_TEST_DATABASE_URL").unwrap()));
    let owner_a_token = bearer(&owner_a);
    let owner_b_token = bearer(&owner_b);

    // A verified human JWT can create a client only in a tenant they own.
    let (status, client) = json_body(send(&app, Request::builder()
        .method("POST")
        .uri(format!("/v1/tenants/{}/api-clients", tenant_a.as_str()))
        .header("authorization", format!("Bearer {owner_a_token}"))
        .header("content-type", "application/json")
        .body(Body::from(json!({
            "displayName":"HTTP integration client",
            "scopes":["identities:read","verification:create"],
            "rateLimitPerMinute":50
        }).to_string())).unwrap()).await).await;
    assert_eq!(status, StatusCode::OK);
    let client_id = client["clientId"].as_str().unwrap().to_owned();

    // Another tenant owner cannot create a credential against this client, even
    // when the attacker substitutes their own tenant ID in the URL.
    let (status, _) = json_body(send(&app, Request::builder()
        .method("POST")
        .uri(format!("/v1/tenants/{}/api-clients/{client_id}/credentials", tenant_b.as_str()))
        .header("authorization", format!("Bearer {owner_b_token}"))
        .header("content-type", "application/json")
        .body(Body::from(json!({
            "scopes":["identities:read"],"environment":"test"
        }).to_string())).unwrap()).await).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, issued) = json_body(send(&app, Request::builder()
        .method("POST")
        .uri(format!("/v1/tenants/{}/api-clients/{client_id}/credentials", tenant_a.as_str()))
        .header("authorization", format!("Bearer {owner_a_token}"))
        .header("content-type", "application/json")
        .body(Body::from(json!({
            "scopes":["identities:read"],"environment":"test"
        }).to_string())).unwrap()).await).await;
    assert_eq!(status, StatusCode::OK);
    let old_secret = issued["secret"].as_str().unwrap().to_owned();
    let old_id = issued["credentialId"].as_str().unwrap().to_owned();

    // Machine authorization is independent from the human JWT and resolves tenant
    // exclusively from the credential record.
    let (status, identity) = json_body(send(&app, Request::builder()
        .method("GET")
        .uri("/v1/machine/whoami")
        .header("authorization", format!("Bearer {old_secret}"))
        .body(Body::empty()).unwrap()).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(identity["tenantId"], tenant_a.as_str());
    assert_eq!(identity["ownerUserId"], owner_a);

    // Cross-tenant human actor cannot rotate another owner's credential.
    let (status, _) = json_body(send(&app, Request::builder()
        .method("POST")
        .uri(format!("/v1/tenants/{}/api-credentials/{old_id}/rotate", tenant_a.as_str()))
        .header("authorization", format!("Bearer {owner_b_token}"))
        .header("content-type", "application/json")
        .body(Body::from(json!({"environment":"test"}).to_string())).unwrap()).await).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Owner rotation revokes the old key and returns a one-time replacement.
    let (status, rotated) = json_body(send(&app, Request::builder()
        .method("POST")
        .uri(format!("/v1/tenants/{}/api-credentials/{old_id}/rotate", tenant_a.as_str()))
        .header("authorization", format!("Bearer {owner_a_token}"))
        .header("content-type", "application/json")
        .body(Body::from(json!({"environment":"test"}).to_string())).unwrap()).await).await;
    assert_eq!(status, StatusCode::OK);
    let new_secret = rotated["secret"].as_str().unwrap();

    let (status, _) = json_body(send(&app, Request::builder()
        .method("GET")
        .uri("/v1/machine/whoami")
        .header("authorization", format!("Bearer {old_secret}"))
        .body(Body::empty()).unwrap()).await).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, identity) = json_body(send(&app, Request::builder()
        .method("GET")
        .uri("/v1/machine/whoami")
        .header("authorization", format!("Bearer {new_secret}"))
        .body(Body::empty()).unwrap()).await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(identity["tenantId"], tenant_a.as_str());

    // A key without identities:read is denied at the HTTP boundary.
    let (status, no_read) = json_body(send(&app, Request::builder()
        .method("POST")
        .uri(format!("/v1/tenants/{}/api-clients/{client_id}/credentials", tenant_a.as_str()))
        .header("authorization", format!("Bearer {owner_a_token}"))
        .header("content-type", "application/json")
        .body(Body::from(json!({
            "scopes":["verification:create"],"environment":"test"
        }).to_string())).unwrap()).await).await;
    assert_eq!(status, StatusCode::OK);
    let no_read_secret = no_read["secret"].as_str().unwrap();
    let (status, _) = json_body(send(&app, Request::builder()
        .method("GET")
        .uri("/v1/machine/whoami")
        .header("authorization", format!("Bearer {no_read_secret}"))
        .body(Body::empty()).unwrap()).await).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
