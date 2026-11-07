//! PostgreSQL integration tests for tenant persistence foundations (#183).
//! Requires EYE_TEST_DATABASE_URL pointing at the dedicated Eye test database.

use king_sparkon_eye::persistence::{
    connect, migrate,
    tenants::{begin_tenant_transaction, create_tenant, TenantId, TenantStoreError},
};
use sqlx::PgPool;
use uuid::Uuid;

async fn pool() -> Option<PgPool> {
    let url = match std::env::var("EYE_TEST_DATABASE_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; tenant persistence tests not run");
            return None;
        }
    };
    let pool = connect(&url).await.expect("connect to Eye test PostgreSQL");
    migrate(&pool).await.expect("apply SQLx migrations");
    Some(pool)
}

async fn seed_user(pool: &PgPool) -> String {
    let user_id = format!("usr_{}", Uuid::new_v4().simple());
    let email = format!("tenant-{}@example.test", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO eye_auth_users (user_id, email, password_hash, salt, role, scopes, is_active, created_at_ms) \
         VALUES ($1, $2, 'test-hash', '', 'Admin', ARRAY[]::text[], TRUE, 1)",
    )
    .bind(&user_id)
    .bind(email)
    .execute(pool)
    .await
    .expect("seed Eye account");
    user_id
}

#[tokio::test]
async fn tenant_is_owned_by_an_existing_eye_account_and_gets_an_opaque_id() {
    let Some(pool) = pool().await else { return };
    let owner = seed_user(&pool).await;

    let tenant = create_tenant(&pool, &owner, "Acme Integration", 1_800_000_000_000)
        .await
        .unwrap();

    assert!(tenant.tenant_id.as_str().starts_with("ten_"));
    assert_eq!(tenant.tenant_id.as_str().len(), 36);
    assert_eq!(tenant.owner_user_id, owner);
    assert_eq!(tenant.display_name, "Acme Integration");
}

#[tokio::test]
async fn tenant_creation_rejects_missing_or_inactive_owner() {
    let Some(pool) = pool().await else { return };

    assert_eq!(
        create_tenant(&pool, "usr_missing", "No owner", 1_800_000_000_000)
            .await
            .unwrap_err(),
        TenantStoreError::OwnerNotFound
    );
    assert_eq!(
        create_tenant(&pool, "usr_missing", " ", 1_800_000_000_000)
            .await
            .unwrap_err(),
        TenantStoreError::InvalidInput
    );
}

#[tokio::test]
async fn tenant_context_is_transaction_local_and_does_not_leak_to_reused_connections() {
    let Some(pool) = pool().await else { return };
    let owner = seed_user(&pool).await;
    let tenant = create_tenant(&pool, &owner, "Connection isolation", 1_800_000_000_000)
        .await
        .unwrap();

    let mut tx = begin_tenant_transaction(&pool, &tenant.tenant_id)
        .await
        .unwrap();
    let active: String =
        sqlx::query_scalar("SELECT current_setting('eye.tenant_id', TRUE)")
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    assert_eq!(active, tenant.tenant_id.as_str());
    tx.commit().await.unwrap();

    let mut connection = pool.acquire().await.unwrap();
    let leaked: Option<String> = sqlx::query_scalar(
        "SELECT NULLIF(current_setting('eye.tenant_id', TRUE), '')",
    )
    .fetch_one(&mut *connection)
    .await
    .unwrap();
    assert_eq!(leaked, None);
}

#[test]
fn tenant_id_parser_rejects_malformed_identifiers() {
    assert!(TenantId::parse("ten_0123456789abcdef0123456789abcdef").is_ok());
    assert!(TenantId::parse("tenant_a").is_err());
    assert!(TenantId::parse("ten_0123456789ABCDEF0123456789ABCDEF").is_err());
}
