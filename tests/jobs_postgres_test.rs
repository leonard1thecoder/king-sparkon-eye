//! PostgreSQL integration tests for durable jobs and result finalization (#187).
//! Requires EYE_TEST_DATABASE_URL pointing at the dedicated Eye test database.
use king_sparkon_eye::persistence::{
    connect, migrate,
    api_credentials::create_api_client,
    identity_registry::{create_enrollment_intent, create_identity, EnrollmentIntentOutcome},
    jobs::{EnqueueOutcome, FailureClass, JobStore},
    tenants::{create_tenant, TenantId},
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

async fn pool() -> Option<PgPool> {
    let url = match std::env::var("EYE_TEST_DATABASE_URL") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => { eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; durable job tests not run"); return None; }
    };
    let pool = connect(&url).await.expect("connect to Eye test PostgreSQL");
    migrate(&pool).await.expect("apply SQLx migrations");
    Some(pool)
}
async fn fixture(pool: &PgPool) -> (TenantId, String) {
    let user = format!("usr_{}", Uuid::new_v4().simple());
    sqlx::query("INSERT INTO eye_auth_users (user_id,email,password_hash,salt,role,scopes,is_active,created_at_ms) VALUES ($1,$2,'test-hash','','Admin',ARRAY[]::text[],TRUE,1)")
        .bind(&user).bind(format!("{user}@example.test")).execute(pool).await.unwrap();
    let tenant = create_tenant(pool, &user, "Durable job test", 1_800_000_000_000).await.unwrap().tenant_id;
    let client = create_api_client(pool, &tenant, &user, "Job test client",
        vec!["identities:read".to_owned(),"identities:write".to_owned()], 600, 1_800_000_000_000)
        .await.unwrap().client_id;
    let identity = create_identity(pool, &tenant, "job-test-subject", json!({}), &client, &user, 1_800_000_000_001).await.unwrap();
    let intent = create_enrollment_intent(pool, &tenant, &identity.identity_id, &client, &user,
        "consent-ref-001", "consent-v1", "intent-key-001", 1_800_000_000_002).await.unwrap();
    let enrollment = match intent { EnrollmentIntentOutcome::Created(v) => v, _ => panic!("new intent") };
    assert_eq!(enrollment.status, "BLOCKED_EVALUATION");
    (tenant, enrollment.enrollment_id)
}
async fn approve_fixture_enrollment(pool: &PgPool, tenant: &TenantId, enrollment: &str) {
    // Test-only simulation of an independently approved evaluation gate.
    sqlx::query("UPDATE eye_identity_enrollments SET status='QUEUED',blocked_reason=NULL,updated_at_ms=created_at_ms+1 WHERE tenant_id=$1 AND enrollment_id=$2")
        .bind(tenant.as_str()).bind(enrollment).execute(pool).await.unwrap();
}

#[tokio::test]
async fn blocked_enrollment_is_not_claimable_and_queued_job_survives_new_pool() {
    let Some(pool) = pool().await else { return };
    let (tenant, enrollment) = fixture(&pool).await;
    let store = JobStore::new(pool.clone());
    assert_eq!(store.enqueue(&tenant,"IDENTITY_ENROLLMENT",&enrollment,"job-key-001",json!({}),1_800_000_000_010,3).await.unwrap_err(),
        king_sparkon_eye::persistence::jobs::JobStoreError::NotFound);
    assert!(store.claim("worker-a",1_800_000_000_011,30_000,10).await.unwrap().is_empty());

    approve_fixture_enrollment(&pool,&tenant,&enrollment).await;
    let (outcome, job) = store.enqueue(&tenant,"IDENTITY_ENROLLMENT",&enrollment,"job-key-001",json!({"policy":"v1"}),1_800_000_000_012,3).await.unwrap();
    assert_eq!(outcome,EnqueueOutcome::Created);
    drop(store);
    let fresh = connect(&std::env::var("EYE_TEST_DATABASE_URL").unwrap()).await.unwrap();
    let recovered = JobStore::new(fresh).get(&tenant,&job.job_id).await.unwrap();
    assert_eq!(recovered.status,"QUEUED");
}

#[tokio::test]
async fn concurrent_workers_claim_disjoint_jobs_and_finalize_once_with_outbox() {
    let Some(pool) = pool().await else { return };
    let (tenant,enrollment)=fixture(&pool).await;
    approve_fixture_enrollment(&pool,&tenant,&enrollment).await;
    let store=JobStore::new(pool.clone());
    let (_,job)=store.enqueue(&tenant,"IDENTITY_ENROLLMENT",&enrollment,"job-key-002",json!({}),1_800_000_000_020,3).await.unwrap();
    let (a,b)=tokio::join!(
        store.claim("worker-a",1_800_000_000_021,30_000,10),
        store.claim("worker-b",1_800_000_000_021,30_000,10)
    );
    let mut claims=a.unwrap(); claims.extend(b.unwrap());
    assert_eq!(claims.len(),1,"SKIP LOCKED must yield one owner for a single job");
    let claim=claims.remove(0);
    assert_eq!(claim.job.job_id,job.job_id);
    assert_eq!(store.heartbeat(&job.job_id,claim.lease_token,"worker-a",1_800_000_000_022,30_000).await.unwrap(),1_800_000_030_022);
    assert!(store.finalize(&job.job_id,claim.lease_token,"worker-a","COMPLETED",Some(json!({"outcome":"inconclusive"})),1_800_000_000_023).await.unwrap());
    assert_eq!(store.finalize(&job.job_id,claim.lease_token,"worker-a","COMPLETED",Some(json!({"outcome":"verified"})),1_800_000_000_024).await.unwrap_err(),
        king_sparkon_eye::persistence::jobs::JobStoreError::LeaseLost);
    let done=store.get(&tenant,&job.job_id).await.unwrap();
    assert_eq!(done.status,"COMPLETED");
    let events=store.claim_outbox("outbox-worker",1_800_000_000_025,30_000,10).await.unwrap();
    assert_eq!(events.len(),1);
    assert_eq!(events[0].job_id,job.job_id);
    assert!(store.acknowledge_outbox(&events[0].event_id,events[0].lease_token,1_800_000_000_026).await.unwrap());
    assert!(!store.acknowledge_outbox(&events[0].event_id,events[0].lease_token,1_800_000_000_027).await.unwrap());
}

#[tokio::test]
async fn retryable_infrastructure_failure_requeues_but_permanent_failure_is_terminal() {
    let Some(pool)=pool().await else { return };
    let (tenant,enrollment)=fixture(&pool).await;
    approve_fixture_enrollment(&pool,&tenant,&enrollment).await;
    let store=JobStore::new(pool);
    let (_,job)=store.enqueue(&tenant,"IDENTITY_ENROLLMENT",&enrollment,"job-key-003",json!({}),1_800_000_000_030,2).await.unwrap();
    let claim=store.claim("worker-a",1_800_000_000_031,30_000,1).await.unwrap().remove(0);
    assert_eq!(store.fail(&job.job_id,claim.lease_token,"worker-a","INFERENCE_TIMEOUT",FailureClass::Retryable,1_800_000_000_032,1000).await.unwrap(),"QUEUED");
    let retry=store.claim("worker-b",1_800_000_001_033,30_000,1).await.unwrap().remove(0);
    assert_eq!(store.fail(&job.job_id,retry.lease_token,"worker-b","MODEL_UNAVAILABLE",FailureClass::Permanent,1_800_000_001_034,0).await.unwrap(),"FAILED");
    assert_eq!(store.get(&tenant,&job.job_id).await.unwrap().status,"FAILED");
}
