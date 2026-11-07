//! PostgreSQL integration tests for tenant-scoped privacy controls (#189).
//! Requires EYE_TEST_DATABASE_URL pointing at a dedicated Eye test database.
use king_sparkon_eye::persistence::{
    connect,migrate,
    privacy::{PrivacyStore,PrivacyStoreError},
    tenants::{TenantId,create_tenant},
};
use serde_json::json;
use sqlx::{PgPool, Row};
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
    let store=PrivacyStore::new(pool);
    store.set_retention_policy(&ta,"EVIDENCE_METADATA",86_400_000,"evidence-v1",1_800_000_000_000,1_800_000_000_000).await.unwrap();
    let evidence=store.register_evidence_reference(&ta,"IDENTITY","idy_test","SUPPORTING_EVIDENCE",
        "private/evidence/object-1",json!({"source":"test"}),"USER","usr-requester",1_800_000_000_001).await.unwrap();
    assert_eq!(store.get_evidence(&tb,&evidence).await.unwrap_err(),PrivacyStoreError::NotFound);
    let first=store.request_deletion(&ta,"EVIDENCE",&evidence,"SUBJECT_REQUEST","USER","usr-requester","delete-key-1",1_800_000_000_010).await.unwrap();
    let replay=store.request_deletion(&ta,"EVIDENCE",&evidence,"SUBJECT_REQUEST","USER","usr-requester","delete-key-1",1_800_000_000_011).await.unwrap();
    assert_eq!(first.deletion_id,replay.deletion_id);
    assert_eq!(store.get_evidence(&ta,&evidence).await.unwrap().status,"DELETE_PENDING");
    let claims=store.claim_deletions(&ta,"cleanup-worker-1",1_800_000_000_020,30_000,10).await.unwrap();
    assert_eq!(claims.len(),1);
    assert_eq!(claims[0].storage_key.as_deref(),Some("private/evidence/object-1"));
    let claim=&claims[0];
    let token=claim.request.lease_token.unwrap();
    assert!(!store.complete_deletion(&ta,&claim.request.deletion_id,Uuid::new_v4(),"cleanup-worker-1",1_800_000_000_021).await.unwrap());
    assert!(store.complete_deletion(&ta,&claim.request.deletion_id,token,"cleanup-worker-1",1_800_000_000_021).await.unwrap());
    let evidence_after=store.get_evidence(&ta,&evidence).await.unwrap();
    assert_eq!(evidence_after.status,"DELETED");
    assert!(evidence_after.deleted_at_ms.is_some());
}


#[tokio::test]
async fn privacy_tables_enforce_transaction_local_tenant_rls(){
    let Some(pool)=pool().await else{return};
    let (is_superuser,bypass_rls):(bool,bool)=sqlx::query_as(
        "SELECT rolsuper,rolbypassrls FROM pg_roles WHERE rolname=current_user"
    ).fetch_one(&pool).await.unwrap();
    assert!(!is_superuser,"test connection must not be superuser");
    assert!(!bypass_rls,"test connection must not have BYPASSRLS");

    let owner_a=owner(&pool).await;let owner_b=owner(&pool).await;
    let tenant_a=tenant(&pool,&owner_a).await;let tenant_b=tenant(&pool,&owner_b).await;
    let store=PrivacyStore::new(pool.clone());
    store.set_retention_policy(&tenant_a,"EVIDENCE_METADATA",86_400_000,"rls-policy-v1",1_800_000_000_100,1_800_000_000_010).await.unwrap();
    let evidence=store.register_evidence_reference(&tenant_a,"IDENTITY","idy_rls_subject","SUPPORTING_EVIDENCE",
        "private/rls/evidence",json!({"source":"rls-test"}),"USER","usr-rls-test",1_800_000_000_011).await.unwrap();

    let visible_without_context:i64=sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM eye_retention_policies)+(SELECT count(*) FROM eye_audit_events)+(SELECT count(*) FROM eye_evidence_references)+(SELECT count(*) FROM eye_deletion_requests)"
    ).fetch_one(&pool).await.unwrap();
    assert_eq!(visible_without_context,0,"privacy tables must fail closed without tenant context");

    let mut tx=king_sparkon_eye::persistence::tenants::begin_tenant_transaction(&pool,&tenant_b).await.unwrap();
    let visible_b:i64=sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM eye_retention_policies WHERE tenant_id=$1)+(SELECT count(*) FROM eye_audit_events WHERE tenant_id=$1)+(SELECT count(*) FROM eye_evidence_references WHERE evidence_id=$2)+(SELECT count(*) FROM eye_deletion_requests WHERE tenant_id=$1)"
    ).bind(tenant_a.as_str()).bind(&evidence).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(visible_b,0,"tenant B must not see tenant A privacy resources");
    let attempted=sqlx::query(
        "INSERT INTO eye_evidence_references(evidence_id,tenant_id,resource_kind,resource_ref,data_class,storage_key,metadata,created_at_ms) VALUES($1,$2,'IDENTITY','idy_other','SUPPORTING_EVIDENCE','private/other','{}'::jsonb,$3)"
    ).bind(format!("evd_{}",Uuid::new_v4().simple())).bind(tenant_a.as_str()).bind(1_800_000_000_200_i64)
     .execute(&mut *tx).await;
    assert!(attempted.is_err(),"tenant B context must reject tenant A evidence writes");
    assert_eq!(attempted.unwrap_err().as_database_error().and_then(|db|db.code()).as_deref(),Some("42501"));
    tx.rollback().await.unwrap();
}


#[tokio::test]
async fn evidence_access_is_authorized_tenant_scoped_and_audited_atomically(){
    let Some(pool)=pool().await else{return};
    let owner_a=owner(&pool).await;let owner_b=owner(&pool).await;
    let tenant_a=tenant(&pool,&owner_a).await;let tenant_b=tenant(&pool,&owner_b).await;
    let store=PrivacyStore::new(pool.clone());
    store.set_retention_policy(&tenant_a,"EVIDENCE_METADATA",86_400_000,
        "evidence-access-v1",1_800_000_000_000,1_800_000_000_000).await.unwrap();
    let evidence_id=store.register_evidence_reference(&tenant_a,"IDENTITY","idy_access_subject",
        "SUPPORTING_EVIDENCE","private/evidence/access-object",json!({"source":"access-test"}),
        "API_CLIENT","cli_evidence_reader",1_800_000_000_001).await.unwrap();

    let accessed=store.get_evidence_for_access(&tenant_a,&evidence_id,"cli_evidence_reader",
        Some("req_evidence_access_1"),1_800_000_000_010).await.unwrap();
    assert_eq!(accessed.evidence_id,evidence_id);
    assert_eq!(accessed.status,"ACTIVE");
    // Evidence metadata DTOs must never contain the private storage key.
    assert!(!accessed.metadata.to_string().contains("private/evidence/access-object"));

    let mut tx=king_sparkon_eye::persistence::tenants::begin_tenant_transaction(&pool,&tenant_a).await.unwrap();
    let audit=sqlx::query("SELECT actor_kind,actor_ref,action,resource_kind,resource_ref,request_id,outcome,metadata FROM eye_audit_events WHERE tenant_id=$1 AND action='EVIDENCE_ACCESSED' AND resource_ref=$2")
        .bind(tenant_a.as_str()).bind(&evidence_id).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(audit.try_get::<String,_>("actor_kind").unwrap(),"API_CLIENT");
    assert_eq!(audit.try_get::<String,_>("actor_ref").unwrap(),"cli_evidence_reader");
    assert_eq!(audit.try_get::<String,_>("request_id").unwrap(),"req_evidence_access_1");
    assert_eq!(audit.try_get::<String,_>("outcome").unwrap(),"SUCCESS");
    assert!(!audit.try_get::<serde_json::Value,_>("metadata").unwrap().to_string().contains("storage"));
    tx.commit().await.unwrap();

    assert_eq!(store.get_evidence_for_access(&tenant_b,&evidence_id,"cli_other_tenant",
        Some("req_cross_tenant"),1_800_000_000_011).await.unwrap_err(),PrivacyStoreError::NotFound);
    let mut tx=king_sparkon_eye::persistence::tenants::begin_tenant_transaction(&pool,&tenant_a).await.unwrap();
    let audit_count:i64=sqlx::query_scalar("SELECT count(*) FROM eye_audit_events WHERE tenant_id=$1 AND action='EVIDENCE_ACCESSED' AND request_id='req_cross_tenant'")
        .bind(tenant_a.as_str()).fetch_one(&mut *tx).await.unwrap();
    assert_eq!(audit_count,0,"denied cross-tenant reads must not create success audit events");
    tx.rollback().await.unwrap();
}


#[tokio::test]
async fn evidence_metadata_rejects_private_object_locations_and_urls(){
    let Some(pool)=pool().await else{return};
    let owner=owner(&pool).await;let tenant=tenant(&pool,&owner).await;
    let store=PrivacyStore::new(pool);
    store.set_retention_policy(&tenant,"EVIDENCE_METADATA",86_400_000,
        "evidence-metadata-v1",1_800_000_000_000,1_800_000_000_000).await.unwrap();
    for metadata in [
        json!({"objectKey":"private/evidence/object-1"}),
        json!({"downloadUrl":"https://bucket.example/object?X-Amz-Signature=secret"}),
        json!({"nested":{"s3_key":"private/evidence/object-2"}}),
    ] {
        let result=store.register_evidence_reference(&tenant,"IDENTITY","idy_metadata_subject",
            "SUPPORTING_EVIDENCE","private/evidence/object-safe",metadata,
            "API_CLIENT","cli_metadata_writer",1_800_000_000_001).await;
        assert_eq!(result.unwrap_err(),PrivacyStoreError::InvalidInput);
    }
}
