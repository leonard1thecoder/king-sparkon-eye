//! PostgreSQL integration tests for tenant-scoped privacy controls (#189).
//! Requires EYE_TEST_DATABASE_URL pointing at a dedicated Eye test database.
use king_sparkon_eye::persistence::{
    connect,migrate,
    privacy::{PrivacyStore,PrivacyStoreError},
    tenants::{TenantId,create_tenant},
};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

async fn pool()->Option<PgPool>{
    let url=match std::env::var("EYE_TEST_DATABASE_URL"){
        Ok(v) if !v.trim().is_empty()=>v,
        _=>{eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; privacy tests not run");return None;}
    };
    let pool=connect(&url).await.expect("connect to Eye test PostgreSQL");
    migrate(&pool).await.expect("apply SQLx migrations");Some(pool)
}
async fn owner(pool:&PgPool)->String{
    let id=format!("usr_{}",Uuid::new_v4().simple());
    let email=format!("privacy-{}@example.test",Uuid::new_v4().simple());
    sqlx::query("INSERT INTO eye_auth_users(user_id,email,password_hash,salt,role,scopes,is_active,created_at_ms) VALUES($1,$2,'test-hash','','Admin',ARRAY[]::text[],TRUE,1800000000000)")
        .bind(&id).bind(email).execute(pool).await.expect("seed owner");id
}
async fn tenant(pool:&PgPool,owner:&str)->TenantId{
    create_tenant(pool,owner,"Privacy test tenant",1_800_000_000_001).await.expect("create tenant").tenant_id
}

#[tokio::test]
async fn retention_policy_is_explicit_tenant_scoped_and_versioned(){
    let Some(pool)=pool().await else{return};
    let a=owner(&pool).await;let b=owner(&pool).await;
    let ta=tenant(&pool,&a).await;let tb=tenant(&pool,&b).await;
    let store=PrivacyStore::new(pool);
    let policy=store.set_retention_policy(&ta,"ORIGINAL_CAPTURE",86_400_000,"policy-v1",1_800_000_000_100,1_800_000_000_010).await.unwrap();
    assert_eq!(policy.retention_ms,86_400_000);
    assert_eq!(store.active_retention_policy(&ta,"ORIGINAL_CAPTURE",1_800_000_000_101).await.unwrap().policy_id,policy.policy_id);
    assert_eq!(store.active_retention_policy(&tb,"ORIGINAL_CAPTURE",1_800_000_000_101).await.unwrap_err(),PrivacyStoreError::NotFound);
}

#[tokio::test]
async fn audit_metadata_rejects_biometric_and_secret_fields(){
    let Some(pool)=pool().await else{return};
    let owner=owner(&pool).await;let tenant=tenant(&pool,&owner).await;
    let store=PrivacyStore::new(pool);
    let result=store.record_audit_event(&tenant,"SYSTEM","privacy-worker","RETENTION_POLICY_SET","RETENTION_POLICY","ret_test",None,"SUCCESS",json!({"policyVersion":"v1"}),1_800_000_000_100).await;
    assert!(result.is_ok());
    let rejected=store.record_audit_event(&tenant,"SYSTEM","privacy-worker","EVIDENCE_ACCESSED","EVIDENCE","evd_test",None,"SUCCESS",json!({"imageBytes":"do-not-store"}),1_800_000_000_101).await;
    assert_eq!(rejected.unwrap_err(),PrivacyStoreError::InvalidInput);
    let rejected=store.record_audit_event(&tenant,"SYSTEM","privacy-worker","EVIDENCE_ACCESSED","EVIDENCE","evd_test",None,"SUCCESS",json!({"nested":{"accessToken":"secret"}}),1_800_000_000_102).await;
    assert_eq!(rejected.unwrap_err(),PrivacyStoreError::InvalidInput);
}

#[tokio::test]
async fn deletion_is_idempotent_tenant_scoped_and_lease_fenced(){
    let Some(pool)=pool().await else{return};
    let owner_a=owner(&pool).await;let owner_b=owner(&pool).await;
    let ta=tenant(&pool,&owner_a).await;let tb=tenant(&pool,&owner_b).await;
    let evidence=format!("evd_{}",Uuid::new_v4().simple());
    sqlx::query("INSERT INTO eye_evidence_references(evidence_id,tenant_id,resource_kind,resource_ref,data_class,storage_key,status,retention_expires_at_ms,created_at_ms) VALUES($1,$2,'IDENTITY','idy_test','SUPPORTING_EVIDENCE','private/evidence/object-1','ACTIVE',$3,$4)")
        .bind(&evidence).bind(ta.as_str()).bind(1_900_000_000_000_i64).bind(1_800_000_000_001_i64)
        .execute(&pool).await.expect("seed evidence");
    let store=PrivacyStore::new(pool);
    assert_eq!(store.get_evidence(&tb,&evidence).await.unwrap_err(),PrivacyStoreError::NotFound);
    let first=store.request_deletion(&ta,"EVIDENCE",&evidence,"SUBJECT_REQUEST","USER","usr-requester","delete-key-1",1_800_000_000_010).await.unwrap();
    let replay=store.request_deletion(&ta,"EVIDENCE",&evidence,"SUBJECT_REQUEST","USER","usr-requester","delete-key-1",1_800_000_000_011).await.unwrap();
    assert_eq!(first.deletion_id,replay.deletion_id);
    assert_eq!(store.get_evidence(&ta,&evidence).await.unwrap().status,"DELETE_PENDING");
    let claims=store.claim_deletions(&ta,"cleanup-worker-1",1_800_000_000_020,30_000,10).await.unwrap();
    assert_eq!(claims.len(),1);
    let claim=&claims[0];
    let token=claim.request.lease_token.unwrap();
    assert!(!store.complete_deletion(&ta,&claim.request.deletion_id,Uuid::new_v4(),"cleanup-worker-1",1_800_000_000_021).await.unwrap());
    assert!(store.complete_deletion(&ta,&claim.request.deletion_id,token,"cleanup-worker-1",1_800_000_000_021).await.unwrap());
    let evidence_after=store.get_evidence(&ta,&evidence).await.unwrap();
    assert_eq!(evidence_after.status,"DELETED");
    assert!(evidence_after.deleted_at_ms.is_some());
}
