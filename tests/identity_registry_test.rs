//! PostgreSQL integration tests for the tenant-scoped identity registry (#185).
//! Requires EYE_TEST_DATABASE_URL pointing at the dedicated Eye test database.

use king_sparkon_eye::persistence::{
    api_credentials::create_api_client,
    connect, migrate,
    identity_registry::{create_enrollment_intent, create_identity, delete_identity, get_identity, get_enrollment_intent, revoke_identity, EnrollmentIntentOutcome, IdentityStoreError},
    tenants::{TenantId, create_tenant},
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

async fn pool() -> Option<PgPool> {
    let url = match std::env::var("EYE_TEST_DATABASE_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; identity registry tests not run");
            return None;
        }
    };
    let pool = connect(&url).await.expect("connect to Eye test PostgreSQL");
    migrate(&pool).await.expect("apply SQLx migrations");
    Some(pool)
}

async fn seed_user(pool: &PgPool) -> String {
    let id = format!("usr_{}", Uuid::new_v4().simple());
    let email = format!("identity-{}@example.test", Uuid::new_v4().simple());
    sqlx::query(
        "INSERT INTO eye_auth_users (user_id,email,password_hash,salt,role,scopes,is_active,created_at_ms) VALUES ($1,$2,'test-hash','','Admin',ARRAY[]::text[],TRUE,1)",
    )
    .bind(&id).bind(email).execute(pool).await.expect("seed user");
    id
}

async fn tenant(pool: &PgPool, owner: &str) -> TenantId {
    create_tenant(pool, owner, "Identity test tenant", 1_800_000_000_000)
        .await.expect("create tenant").tenant_id
}

async fn client(pool: &PgPool, tenant: &TenantId, owner: &str) -> String {
    create_api_client(
        pool, tenant, owner, "Identity test client",
        vec!["identities:read".to_owned(), "identities:write".to_owned()],
        600, 1_800_000_000_000,
    ).await.expect("create API client").client_id
}

#[tokio::test]
async fn identities_are_tenant_unique_and_cross_tenant_reads_are_non_disclosing() {
    let Some(pool) = pool().await else { return };
    let owner_a = seed_user(&pool).await;
    let owner_b = seed_user(&pool).await;
    let tenant_a = tenant(&pool, &owner_a).await;
    let tenant_b = tenant(&pool, &owner_b).await;
    let _client_a = client(&pool, &tenant_a, &owner_a).await;
    let _client_b = client(&pool, &tenant_b, &owner_b).await;

    let identity = create_identity(
        &pool, &tenant_a, "external-user-42", json!({"source":"integration"}),
        &_client_a, &owner_a, 1_800_000_000_001,
    ).await.expect("create identity");

    assert_eq!(
        get_identity(&pool, &tenant_b, &identity.identity_id).await.unwrap_err(),
        IdentityStoreError::NotFound
    );
    assert_eq!(
        create_identity(&pool, &tenant_a, "external-user-42", json!({}),
            &_client_a, &owner_a, 1_800_000_000_002)
            .await.unwrap_err(),
        IdentityStoreError::Conflict
    );
    // The same external reference is valid in a different tenant.
    let other = create_identity(
        &pool, &tenant_b, "external-user-42", json!({}),
        &_client_b, &owner_b, 1_800_000_000_003,
    ).await.expect("tenant-local external reference");
    assert_ne!(identity.identity_id, other.identity_id);
}

#[tokio::test]
async fn revocation_and_deletion_are_audited_and_non_reactivating() {
    let Some(pool) = pool().await else { return };
    let owner = seed_user(&pool).await;
    let tenant_id = tenant(&pool, &owner).await;
    let client_id = client(&pool, &tenant_id, &owner).await;
    let identity = create_identity(
        &pool, &tenant_id, "revocable-subject", json!({}),
        &client_id, &owner, 1_800_000_000_010,
    ).await.expect("create identity");

    let revoked = revoke_identity(
        &pool, &tenant_id, &identity.identity_id, &client_id, &owner, 1_800_000_000_011,
    ).await.expect("revoke identity");
    assert_eq!(revoked.status, "REVOKED");
    assert_eq!(revoked.enrollment_status, "REVOKED");
    assert_eq!(
        revoke_identity(&pool, &tenant_id, &identity.identity_id, &client_id, &owner, 1_800_000_000_012)
            .await.unwrap_err(),
        IdentityStoreError::NotFound
    );

    delete_identity(
        &pool, &tenant_id, &identity.identity_id, &client_id, &owner, 1_800_000_000_013,
    ).await.expect("delete identity");
    assert_eq!(
        get_identity(&pool, &tenant_id, &identity.identity_id).await.unwrap_err(),
        IdentityStoreError::NotFound
    );

    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM eye_identity_audit WHERE tenant_id=$1 AND identity_id=$2 ORDER BY audit_id",
    ).bind(tenant_id.as_str()).bind(&identity.identity_id)
        .fetch_all(&pool).await.expect("read identity audit");
    assert_eq!(actions, vec!["IDENTITY_CREATED", "IDENTITY_REVOKED", "IDENTITY_DELETED"]);
}

#[tokio::test]
async fn enrollment_intents_are_durable_idempotent_and_blocked_by_evaluation_gate() {
    let Some(pool) = pool().await else { return };
    let owner_a = seed_user(&pool).await;
    let owner_b = seed_user(&pool).await;
    let tenant_a = tenant(&pool, &owner_a).await;
    let tenant_b = tenant(&pool, &owner_b).await;
    let client_a = client(&pool, &tenant_a, &owner_a).await;
    let _client_b = client(&pool, &tenant_b, &owner_b).await;
    let identity = create_identity(
        &pool, &tenant_a, "enrollment-subject", json!({}),
        &client_a, &owner_a, 1_800_000_000_100,
    ).await.expect("create identity");

    let created = create_enrollment_intent(
        &pool, &tenant_a, &identity.identity_id, &client_a, &owner_a,
        "consent-ref-001", "consent-policy-v1", "enroll-key-001", 1_800_000_000_101,
    ).await.expect("persist blocked enrollment intent");
    let enrollment = match created {
        EnrollmentIntentOutcome::Created(value) => value,
        EnrollmentIntentOutcome::Existing(_) => panic!("first request must create"),
    };
    assert_eq!(enrollment.status, "BLOCKED_EVALUATION");
    assert_eq!(enrollment.blocked_reason.as_deref(), Some("EVALUATION_DATA_REQUIRED"));

    let replay = create_enrollment_intent(
        &pool, &tenant_a, &identity.identity_id, &client_a, &owner_a,
        "consent-ref-001", "consent-policy-v1", "enroll-key-001", 1_800_000_000_102,
    ).await.expect("idempotent replay");
    let replayed = match replay {
        EnrollmentIntentOutcome::Existing(value) => value,
        EnrollmentIntentOutcome::Created(_) => panic!("retry must return existing resource"),
    };
    assert_eq!(replayed.enrollment_id, enrollment.enrollment_id);

    assert_eq!(
        create_enrollment_intent(
            &pool, &tenant_a, &identity.identity_id, &client_a, &owner_a,
            "different-consent-001", "consent-policy-v1", "enroll-key-001", 1_800_000_000_103,
        ).await.unwrap_err(),
        IdentityStoreError::Conflict
    );

    assert_eq!(
        get_enrollment_intent(&pool, &tenant_b, &identity.identity_id, &enrollment.enrollment_id)
            .await.unwrap_err(),
        IdentityStoreError::NotFound
    );

    let identity_after = get_identity(&pool, &tenant_a, &identity.identity_id)
        .await.expect("read identity state");
    assert_eq!(identity_after.enrollment_status, "BLOCKED_EVALUATION");

    let persisted_status: String = sqlx::query_scalar(
        "SELECT status FROM eye_identity_enrollments WHERE tenant_id=$1 AND enrollment_id=$2",
    ).bind(tenant_a.as_str()).bind(&enrollment.enrollment_id)
        .fetch_one(&pool).await.expect("read durable enrollment");
    assert_eq!(persisted_status, "BLOCKED_EVALUATION");
}
