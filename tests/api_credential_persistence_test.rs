//! PostgreSQL integration tests for tenant-bound API clients and credentials (#184).
//! Requires EYE_TEST_DATABASE_URL pointing at the dedicated Eye test database.

use king_sparkon_eye::persistence::{
    api_credentials::{
        ApiCredentialError, authenticate_api_credential, consume_rate_limit, create_api_client,
        create_api_credential, revoke_api_credential, rotate_api_credential,
    },
    connect, migrate,
    tenants::{TenantId, create_tenant},
};
use sqlx::PgPool;
use uuid::Uuid;

async fn pool() -> Option<PgPool> {
    let url = match std::env::var("EYE_TEST_DATABASE_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; API credential tests not run");
            return None;
        }
    };
    let pool = connect(&url).await.expect("connect to Eye test PostgreSQL");
    migrate(&pool).await.expect("apply SQLx migrations");
    Some(pool)
}

async fn seed_owner(pool: &PgPool) -> String {
    let user_id = format!("usr_{}", Uuid::new_v4().simple());
    let email = format!("api-client-{}@example.test", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO eye_auth_users (user_id, email, password_hash, salt, role, scopes, is_active, created_at_ms) VALUES ($1,$2,'test-hash','', 'Admin', ARRAY[]::text[], TRUE, 1)",
    )
    .bind(&user_id)
    .bind(email)
    .execute(pool)
    .await
    .expect("seed Eye account");
    user_id
}

async fn owned_tenant(pool: &PgPool, owner: &str, name: &str) -> TenantId {
    create_tenant(pool, owner, name, 1_800_000_000_000)
        .await
        .expect("create tenant")
        .tenant_id
}

fn scopes(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[tokio::test]
async fn credential_resolves_tenant_from_database_and_enforces_scopes() {
    let Some(pool) = pool().await else { return };
    let owner = seed_owner(&pool).await;
    let tenant = owned_tenant(&pool, &owner, "Tenant-bound API").await;
    let client = create_api_client(
        &pool,
        &tenant,
        &owner,
        "Production integration",
        scopes(&["identities:read", "verification:create"]),
        120,
        1_800_000_000_100,
    )
    .await
    .unwrap();

    let created = create_api_credential(
        &pool,
        &tenant,
        &client.client_id,
        &owner,
        "test",
        scopes(&["identities:read"]),
        Some(60_000),
        1_800_000_000_200,
    )
    .await
    .unwrap();

    let authenticated = authenticate_api_credential(
        &pool,
        &created.raw_secret,
        Some("identities:read"),
        1_800_000_000_300,
    )
    .await
    .unwrap();
    assert_eq!(authenticated.tenant_id, tenant);
    assert_eq!(authenticated.client_id, client.client_id);
    assert_eq!(authenticated.owner_user_id, owner);
    assert_eq!(authenticated.scopes, scopes(&["identities:read"]));

    assert_eq!(
        authenticate_api_credential(
            &pool,
            &created.raw_secret,
            Some("verification:create"),
            1_800_000_000_300,
        )
        .await,
        Err(ApiCredentialError::InsufficientScope)
    );

    let (stored_hash, stored_salt): (String, String) =
        sqlx::query_as("SELECT key_hash, salt FROM eye_api_credentials WHERE credential_id=$1")
            .bind(&created.credential_id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_ne!(stored_hash, created.raw_secret);
    assert!(!stored_hash.contains(&created.raw_secret));
    assert!(!stored_salt.is_empty());
    assert!(!format!("{created:?}").contains(&created.raw_secret));
}

#[tokio::test]
async fn client_and_credential_scopes_cannot_exceed_the_parent_grant() {
    let Some(pool) = pool().await else { return };
    let owner = seed_owner(&pool).await;
    let tenant = owned_tenant(&pool, &owner, "Scope boundary").await;
    let client = create_api_client(
        &pool,
        &tenant,
        &owner,
        "Read-only client",
        scopes(&["identities:read"]),
        60,
        1_800_000_000_100,
    )
    .await
    .unwrap();

    assert_eq!(
        create_api_credential(
            &pool,
            &tenant,
            &client.client_id,
            &owner,
            scopes(&["identities:write"]),
            None,
            1_800_000_000_200,
        )
        .await,
        Err(ApiCredentialError::InsufficientScope)
    );
}

#[tokio::test]
async fn another_tenant_owner_cannot_create_or_revoke_credentials_for_a_client() {
    let Some(pool) = pool().await else { return };
    let owner_a = seed_owner(&pool).await;
    let owner_b = seed_owner(&pool).await;
    let tenant_a = owned_tenant(&pool, &owner_a, "Tenant A").await;
    let tenant_b = owned_tenant(&pool, &owner_b, "Tenant B").await;
    let client_a = create_api_client(
        &pool,
        &tenant_a,
        &owner_a,
        "Client A",
        scopes(&["identities:read"]),
        60,
        1_800_000_000_100,
    )
    .await
    .unwrap();
    let credential = create_api_credential(
        &pool,
        &tenant_a,
        &client_a.client_id,
        &owner_a,
        "test",
        scopes(&["identities:read"]),
        None,
        1_800_000_000_200,
    )
    .await
    .unwrap();

    assert_eq!(
        create_api_credential(
            &pool,
            &tenant_b,
            &client_a.client_id,
            &owner_b,
            scopes(&["identities:read"]),
            None,
            1_800_000_000_300,
        )
        .await,
        Err(ApiCredentialError::NotFound)
    );
    assert_eq!(
        revoke_api_credential(
            &pool,
            &tenant_a,
            &credential.credential_id,
            &owner_b,
            1_800_000_000_400,
        )
        .await,
        Err(ApiCredentialError::NotFound)
    );
    assert!(
        authenticate_api_credential(
            &pool,
            &credential.raw_secret,
            Some("identities:read"),
            1_800_000_000_500,
        )
        .await
        .is_ok()
    );
}

#[tokio::test]
async fn rotation_revokes_old_secret_and_returns_new_secret_once() {
    let Some(pool) = pool().await else { return };
    let owner = seed_owner(&pool).await;
    let tenant = owned_tenant(&pool, &owner, "Rotation").await;
    let client = create_api_client(
        &pool,
        &tenant,
        &owner,
        "Rotating client",
        scopes(&["verification:read"]),
        60,
        1_800_000_000_100,
    )
    .await
    .unwrap();
    let old = create_api_credential(
        &pool,
        &tenant,
        &client.client_id,
        &owner,
        "test",
        scopes(&["verification:read"]),
        None,
        1_800_000_000_200,
    )
    .await
    .unwrap();

    let new = rotate_api_credential(
        &pool,
        &tenant,
        &old.credential_id,
        &owner,
        "test",
        None,
        1_800_000_000_300,
    )
    .await
    .unwrap();

    assert_eq!(
        authenticate_api_credential(&pool, &old.raw_secret, None, 1_800_000_000_400,).await,
        Err(ApiCredentialError::InvalidCredential)
    );
    assert!(
        authenticate_api_credential(
            &pool,
            &new.raw_secret,
            Some("verification:read"),
            1_800_000_000_400,
        )
        .await
        .is_ok()
    );
    let rotated_from: Option<String> = sqlx::query_scalar(
        "SELECT rotated_from_credential_id FROM eye_api_credentials WHERE credential_id=$1",
    )
    .bind(&new.credential_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(rotated_from.as_deref(), Some(old.credential_id.as_str()));
}

#[tokio::test]
async fn tenant_and_credential_rate_limits_are_enforced_independently() {
    let Some(pool) = pool().await else { return };
    let owner = seed_owner(&pool).await;
    let tenant = owned_tenant(&pool, &owner, "Rate limits").await;
    let client = create_api_client(
        &pool,
        &tenant,
        &owner,
        "Rate-limited client",
        scopes(&["identities:read"]),
        2,
        1_800_000_000_100,
    )
    .await
    .unwrap();
    let credential = create_api_credential(
        &pool,
        &tenant,
        &client.client_id,
        &owner,
        "test",
        scopes(&["identities:read"]),
        None,
        1_800_000_000_200,
    )
    .await
    .unwrap();

    assert!(
        consume_rate_limit(
            &pool,
            &tenant,
            &credential.credential_id,
            1_800_000_000_300,
            2,
            1,
        )
        .await
        .unwrap()
    );
    assert!(
        !consume_rate_limit(
            &pool,
            &tenant,
            &credential.credential_id,
            1_800_000_000_400,
            2,
            1,
        )
        .await
        .unwrap()
    );
}
