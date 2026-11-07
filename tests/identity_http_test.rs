//! HTTP authorization tests for tenant-scoped identity resources (#185).
//! Requires EYE_TEST_DATABASE_URL pointing at the dedicated Eye PostgreSQL test database.

use axum::{Router, body::{Body, to_bytes}, http::{Request, StatusCode}, response::Response};
use king_sparkon_eye::{
    config::Config,
    identity,
    persistence::{
        api_credentials::{create_api_client, create_api_credential, CreateApiCredentialInput},
        connect, migrate, tenants::{TenantId, create_tenant},
    },
};
use serde_json::{Value, json};
use std::collections::HashMap;
use tower::ServiceExt;
use uuid::Uuid;

fn config(database_url: String) -> Config {
    let values = HashMap::from([
        ("EYE_GOOGLE_CLIENT_ID", "client-id".to_owned()),
        ("EYE_GOOGLE_CLIENT_SECRET", "client-secret".to_owned()),
        ("EYE_GOOGLE_REDIRECT_URL", "https://eye.example.com/v1/auth/google/callback".to_owned()),
        ("EYE_DATABASE_URL", database_url),
        ("EYE_OAUTH_ENCRYPTION_KEY", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".to_owned()),
        ("EYE_OAUTH_CALLBACK_URLS", "https://eye.example.com/auth/callback".to_owned()),
        ("EYE_OAUTH_EXCHANGE_SECRET", "e".repeat(48)),
        ("EYE_JWT_SIGNING_SECRET", "j".repeat(48)),
        ("EYE_JWT_KID", "identity-http-test-kid".to_owned()),
    ]);
    Config::from_lookup(|name| values.get(name).cloned()).expect("valid HTTP test config")
}

async fn test_pool() -> Option<sqlx::PgPool> {
    let Ok(url) = std::env::var("EYE_TEST_DATABASE_URL") else {
        eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; identity HTTP tests not run");
        return None;
    };
    let pool = connect(&url).await.expect("connect test PostgreSQL");
    migrate(&pool).await.expect("apply Eye migrations");
    Some(pool)
}

async fn seed_owner(pool: &sqlx::PgPool) -> String {
    let id = format!("usr_{}", Uuid::new_v4().simple());
    let email = format!("identity-http-{}@example.test", Uuid::new_v4().simple());
    sqlx::query("INSERT INTO eye_auth_users (user_id,email,password_hash,salt,role,scopes,is_active,created_at_ms) VALUES ($1,$2,'test-hash','','Admin',ARRAY[]::text[],TRUE,1)")
        .bind(&id).bind(email).execute(pool).await.expect("seed owner");
    id
}

async fn tenant_and_key(pool: &sqlx::PgPool, owner: &str, scopes: Vec<String>) -> (TenantId, String) {
    let tenant = create_tenant(pool, owner, "Identity HTTP tenant", 1_800_000_000_000)
        .await.expect("create tenant").tenant_id;
    let client = create_api_client(pool, &tenant, owner, "Identity HTTP client", scopes.clone(), 600, 1_800_000_000_000)
        .await.expect("create API client");
    let credential = create_api_credential(pool, CreateApiCredentialInput {
        tenant_id: &tenant,
        client_id: &client.client_id,
        actor_user_id: owner,
        environment: "test",
        scopes,
        ttl_ms: None,
        now_ms: 1_800_000_000_001,
    }).await.expect("create API credential");
    (tenant, credential.raw_secret)
}

async fn send(app: &Router, request: Request<Body>) -> Response {
    app.clone().oneshot(request).await.expect("HTTP response")
}

async fn json_response(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024).await.unwrap();
    let value = if bytes.is_empty() { Value::Null } else { serde_json::from_slice(&bytes).unwrap() };
    (status, value)
}

async fn request_json(app: &Router, method: &str, uri: &str, key: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri)
        .header("authorization", format!("Bearer {key}"));
    if body.is_some() { builder = builder.header("content-type", "application/json"); }
    let request = builder.body(body.map(|v| Body::from(v.to_string())).unwrap_or_else(Body::empty)).unwrap();
    json_response(send(app, request).await).await
}

#[tokio::test]
async fn machine_scope_and_tenant_binding_are_enforced_on_identity_http_routes() {
    let Some(pool) = test_pool().await else { return };
    let owner_a = seed_owner(&pool).await;
    let owner_b = seed_owner(&pool).await;
    let (tenant_a, key_a) = tenant_and_key(&pool, &owner_a, vec!["identities:read".into(), "identities:write".into()]).await;
    let (tenant_b, key_b) = tenant_and_key(&pool, &owner_b, vec!["identities:read".into(), "identities:write".into()]).await;
    let read_only_owner = seed_owner(&pool).await;
    let (_read_only_tenant, read_only_key) = tenant_and_key(&pool, &read_only_owner, vec!["identities:read".into()]).await;
    let app = identity::router(config(std::env::var("EYE_TEST_DATABASE_URL").unwrap()));

    let (status, created) = request_json(&app, "POST", "/v1/identities", &key_a, Some(json!({
        "externalReference":"external-123",
        "metadata":{"source":"integration"}
    }))).await;
    assert_eq!(status, StatusCode::CREATED);
    let identity_id = created["identityId"].as_str().unwrap().to_owned();

    let (status, _) = request_json(&app, "GET", &format!("/v1/identities/{identity_id}"), &key_b, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, _) = request_json(&app, "POST", "/v1/identities", &read_only_key, Some(json!({
        "externalReference":"forbidden-write"
    }))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, read) = request_json(&app, "GET", &format!("/v1/identities/{identity_id}"), &key_a, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(read.get("tenantId").is_none()); // tenant identifiers are intentionally not returned
    assert_eq!(read["status"], "ACTIVE");

    let (status, revoked) = request_json(&app, "POST", &format!("/v1/identities/{identity_id}/revoke"), &key_a, Some(json!({}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(revoked["status"], "REVOKED");

    let (status, _) = request_json(&app, "DELETE", &format!("/v1/identities/{identity_id}"), &key_a, None).await;
    assert_eq!(status, StatusCode::NO_CONTENT);

    let (status, _) = request_json(&app, "GET", &format!("/v1/identities/{identity_id}"), &key_a, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    assert!(tenant_a.as_str().starts_with("ten_"));
}
