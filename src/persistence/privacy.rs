//! Tenant-scoped audit, evidence and retention persistence (#189).
//!
//! This module stores metadata and durable deletion work only. It never stores biometric
//! bytes, templates, signed URLs, secrets or provider payloads.
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use crate::persistence::tenants::{begin_tenant_transaction, TenantId};

const MAX_BATCH:i64=100;
const MAX_LEASE_MS:i64=300_000;

#[derive(Debug,Clone,PartialEq,Eq,thiserror::Error)]
pub enum PrivacyStoreError {
    #[error("privacy-control input is invalid")] InvalidInput,
    #[error("privacy resource was not found")] NotFound,
    #[error("privacy idempotency key conflicts")] Conflict,
    #[error("privacy persistence is unavailable")] Unavailable,
}
impl From<sqlx::Error> for PrivacyStoreError {
    fn from(error:sqlx::Error)->Self {
        if let sqlx::Error::Database(db)=&error {
            if db.code().as_deref()==Some("23505"){return Self::Conflict;}
        }
        Self::Unavailable
    }
}
fn safe_ref(v:&str,max:usize)->bool {
    !v.is_empty()&&v.len()<=max&&v.trim()==v&&!v.chars().any(char::is_control)
}
fn safe_key(v:&str)->bool {
    !v.is_empty()&&v.len()<=128&&v.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'||b==b'-')
}
fn safe_worker(v:&str)->bool {
    safe_ref(v,128)&&v.bytes().all(|b|b.is_ascii_alphanumeric()||b"_.:-".contains(&b))
}
fn safe_metadata(value:&Value)->bool {
    fn walk(v:&Value)->bool {
        match v {
            Value::Object(map)=>map.iter().all(|(k,v)|{
                let key=k.to_ascii_lowercase();
                !["image","photo","biometric","embedding","template","token","secret","password","payload","signed_url","signedurl","presigned","storage_key","storagekey","object_key","objectkey","s3_key","bucket","url","uri"]
                    .iter().any(|blocked|key.contains(blocked))&&walk(v)
            }),
            Value::Array(items)=>items.iter().all(walk),
            Value::String(s)=>s.len()<=512&&!s.chars().any(char::is_control),
            _=>true,
        }
    }
    value.is_object()&&serde_json::to_vec(value).is_ok_and(|v|v.len()<=2048)&&walk(value)
}

#[derive(Debug,Clone,PartialEq,Eq)]
pub struct RetentionPolicy {
    pub policy_id:String,pub data_class:String,pub retention_ms:i64,
    pub policy_version:String,pub effective_at_ms:i64,
}
#[derive(Debug,Clone,PartialEq)]
pub struct EvidenceReference {
    pub evidence_id:String,pub resource_kind:String,pub resource_ref:String,
    pub data_class:String,pub status:String,pub retention_expires_at_ms:Option<i64>,
    pub metadata:Value,pub created_at_ms:i64,pub deleted_at_ms:Option<i64>,
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct DeletionRequest {
    pub deletion_id:String,pub target_kind:String,pub target_ref:String,pub reason_code:String,
    pub status:String,pub attempts:i32,pub max_attempts:i32,pub lease_token:Option<Uuid>,
    pub lease_until_ms:Option<i64>,
}
#[derive(Debug,Clone,PartialEq,Eq)]
pub struct ClaimedDeletion {
    pub request:DeletionRequest,pub tenant_id:TenantId,pub worker_id:String,
    /// Private object key for the trusted cleanup worker only; never returned by evidence reads.
    pub storage_key:Option<String>,
}

#[derive(Clone)]
pub struct PrivacyStore{pool:PgPool}
impl PrivacyStore {
    pub fn new(pool:PgPool)->Self{Self{pool}}

    /// Replaces the active policy for one data class. No implicit/default retention is applied.
    pub async fn set_retention_policy(&self,tenant:&TenantId,data_class:&str,retention_ms:i64,
        version:&str,effective_at_ms:i64,now_ms:i64)->Result<RetentionPolicy,PrivacyStoreError>{
        if !matches!(data_class,"ORIGINAL_CAPTURE"|"FACE_TEMPLATE"|"EVIDENCE_METADATA"|"AUDIT_RECORD")
            ||retention_ms<=0||!safe_ref(version,128)
            ||effective_at_ms<=0||now_ms<=0||effective_at_ms>now_ms {
            return Err(PrivacyStoreError::InvalidInput);
        }
        let mut tx=begin_tenant_transaction(&self.pool, tenant).await?;
        sqlx::query("UPDATE eye_retention_policies SET active=FALSE WHERE tenant_id=$1 AND data_class=$2 AND active=TRUE")
            .bind(tenant.as_str()).bind(data_class).execute(&mut *tx).await?;
        let id=format!("ret_{}",Uuid::new_v4().simple());
        let row=sqlx::query("INSERT INTO eye_retention_policies(policy_id,tenant_id,data_class,retention_ms,policy_version,effective_at_ms,active,created_at_ms) VALUES($1,$2,$3,$4,$5,$6,TRUE,$7) RETURNING policy_id,data_class,retention_ms,policy_version,effective_at_ms")
            .bind(&id).bind(tenant.as_str()).bind(data_class).bind(retention_ms).bind(version).bind(effective_at_ms).bind(now_ms)
            .fetch_one(&mut *tx).await?;
        let policy=RetentionPolicy{policy_id:row.try_get("policy_id")?,data_class:row.try_get("data_class")?,
            retention_ms:row.try_get("retention_ms")?,policy_version:row.try_get("policy_version")?,
            effective_at_ms:row.try_get("effective_at_ms")?};
        sqlx::query("INSERT INTO eye_audit_events(tenant_id,actor_kind,actor_ref,action,resource_kind,resource_ref,outcome,metadata,created_at_ms) VALUES($1,'SYSTEM','privacy-policy-manager','RETENTION_POLICY_SET','RETENTION_POLICY',$2,'SUCCESS',$3,$4)")
            .bind(tenant.as_str()).bind(&policy.policy_id).bind(serde_json::json!({"dataClass":data_class,"policyVersion":version,"retentionMs":retention_ms})).bind(now_ms).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(policy)
    }

    pub async fn active_retention_policy(&self,tenant:&TenantId,data_class:&str,now_ms:i64)
        ->Result<RetentionPolicy,PrivacyStoreError>{
        if now_ms<=0||!matches!(data_class,"ORIGINAL_CAPTURE"|"FACE_TEMPLATE"|"EVIDENCE_METADATA"|"AUDIT_RECORD"){
            return Err(PrivacyStoreError::InvalidInput);
        }
        let mut tx=begin_tenant_transaction(&self.pool, tenant).await?;
        let row=sqlx::query("SELECT policy_id,data_class,retention_ms,policy_version,effective_at_ms FROM eye_retention_policies WHERE tenant_id=$1 AND data_class=$2 AND active=TRUE AND effective_at_ms<=$3")
            .bind(tenant.as_str()).bind(data_class).bind(now_ms).fetch_optional(&mut *tx).await?
            .ok_or(PrivacyStoreError::NotFound)?;
        let policy=RetentionPolicy{policy_id:row.try_get("policy_id")?,data_class:row.try_get("data_class")?,
            retention_ms:row.try_get("retention_ms")?,policy_version:row.try_get("policy_version")?,
            effective_at_ms:row.try_get("effective_at_ms")?};
        tx.commit().await?;
        Ok(policy)
    }

    pub async fn record_audit_event(&self,tenant:&TenantId,actor_kind:&str,actor_ref:&str,
        action:&str,resource_kind:&str,resource_ref:&str,request_id:Option<&str>,
        outcome:&str,metadata:Value,now_ms:i64)->Result<i64,PrivacyStoreError>{
        if !matches!(actor_kind,"USER"|"API_CLIENT"|"SYSTEM")||!safe_ref(actor_ref,128)
            ||!safe_ref(action,80)||!action.bytes().all(|b|b.is_ascii_uppercase()||b.is_ascii_digit()||b==b'_')
            ||!safe_ref(resource_kind,80)||!resource_kind.bytes().all(|b|b.is_ascii_uppercase()||b.is_ascii_digit()||b==b'_')
            ||!safe_ref(resource_ref,128)||request_id.is_some_and(|v|!safe_ref(v,128))
            ||!matches!(outcome,"SUCCESS"|"DENIED"|"FAILED")||!safe_metadata(&metadata)||now_ms<=0 {
            return Err(PrivacyStoreError::InvalidInput);
        }
        let mut tx=begin_tenant_transaction(&self.pool, tenant).await?;
        let id=sqlx::query_scalar::<_,i64>("INSERT INTO eye_audit_events(tenant_id,actor_kind,actor_ref,action,resource_kind,resource_ref,request_id,outcome,metadata,created_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING audit_event_id")
            .bind(tenant.as_str()).bind(actor_kind).bind(actor_ref).bind(action).bind(resource_kind)
            .bind(resource_ref).bind(request_id).bind(outcome).bind(metadata).bind(now_ms)
            .fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(id)
    }

    /// Registers an opaque reference to an already-written private object.
    /// A matching tenant retention policy is mandatory before metadata is admitted.
    #[allow(clippy::too_many_arguments)]
    pub async fn register_evidence_reference(&self,tenant:&TenantId,resource_kind:&str,resource_ref:&str,
        data_class:&str,storage_key:&str,metadata:Value,actor_kind:&str,actor_ref:&str,now_ms:i64)
        ->Result<String,PrivacyStoreError>{
        let policy_class=match data_class {
            "ORIGINAL_CAPTURE"=>"ORIGINAL_CAPTURE",
            "FACE_TEMPLATE"=>"FACE_TEMPLATE",
            "SUPPORTING_EVIDENCE"=>"EVIDENCE_METADATA",
            _=>return Err(PrivacyStoreError::InvalidInput),
        };
        if !matches!(resource_kind,"IDENTITY"|"ENROLLMENT"|"VERIFICATION_SESSION")
            ||!safe_ref(resource_ref,128)||!safe_ref(storage_key,512)||storage_key.starts_with('/')
            ||storage_key.contains("..")||storage_key.contains("://")||storage_key.contains('?')||storage_key.contains('#')||!safe_metadata(&metadata)
            ||!matches!(actor_kind,"USER"|"API_CLIENT"|"SYSTEM")||!safe_ref(actor_ref,128)||now_ms<=0 {
            return Err(PrivacyStoreError::InvalidInput);
        }
        let mut tx=begin_tenant_transaction(&self.pool, tenant).await?;
        let retention=sqlx::query_scalar::<_,i64>("SELECT retention_ms FROM eye_retention_policies WHERE tenant_id=$1 AND data_class=$2 AND active=TRUE AND effective_at_ms<=$3")
            .bind(tenant.as_str()).bind(policy_class).bind(now_ms).fetch_optional(&mut *tx).await?
            .ok_or(PrivacyStoreError::NotFound)?;
        let expires=now_ms.checked_add(retention).ok_or(PrivacyStoreError::InvalidInput)?;
        let id=format!("evd_{}",Uuid::new_v4().simple());
        sqlx::query("INSERT INTO eye_evidence_references(evidence_id,tenant_id,resource_kind,resource_ref,data_class,storage_key,status,retention_expires_at_ms,metadata,created_at_ms) VALUES($1,$2,$3,$4,$5,$6,'ACTIVE',$7,$8,$9)")
            .bind(&id).bind(tenant.as_str()).bind(resource_kind).bind(resource_ref).bind(data_class)
            .bind(storage_key).bind(expires).bind(metadata).bind(now_ms).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO eye_audit_events(tenant_id,actor_kind,actor_ref,action,resource_kind,resource_ref,outcome,metadata,created_at_ms) VALUES($1,$2,$3,'EVIDENCE_REGISTERED','EVIDENCE',$4,'SUCCESS',$5,$6)")
            .bind(tenant.as_str()).bind(actor_kind).bind(actor_ref).bind(&id)
            .bind(serde_json::json!({"dataClass":data_class,"retentionExpiresAtMs":expires})).bind(now_ms)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(id)
    }

    pub async fn get_evidence(&self,tenant:&TenantId,evidence_id:&str)->Result<EvidenceReference,PrivacyStoreError>{
        if !evidence_id.starts_with("evd_"){return Err(PrivacyStoreError::InvalidInput);}
        let mut tx=begin_tenant_transaction(&self.pool, tenant).await?;
        let row=sqlx::query("SELECT evidence_id,resource_kind,resource_ref,data_class,status,retention_expires_at_ms,metadata,created_at_ms,deleted_at_ms FROM eye_evidence_references WHERE tenant_id=$1 AND evidence_id=$2")
            .bind(tenant.as_str()).bind(evidence_id).fetch_optional(&mut *tx).await?
            .ok_or(PrivacyStoreError::NotFound)?;
        let evidence=EvidenceReference{evidence_id:row.try_get("evidence_id")?,resource_kind:row.try_get("resource_kind")?,
            resource_ref:row.try_get("resource_ref")?,data_class:row.try_get("data_class")?,status:row.try_get("status")?,
            retention_expires_at_ms:row.try_get("retention_expires_at_ms")?,metadata:row.try_get("metadata")?,
            created_at_ms:row.try_get("created_at_ms")?,deleted_at_ms:row.try_get("deleted_at_ms")?};
        tx.commit().await?;
        Ok(evidence)
    }

    /// Reads active evidence metadata and records access in the same tenant transaction.
    /// The private object key is deliberately excluded from the returned value.
    pub async fn get_evidence_for_access(&self,tenant:&TenantId,evidence_id:&str,
        actor_ref:&str,request_id:Option<&str>,now_ms:i64)->Result<EvidenceReference,PrivacyStoreError>{
        if !evidence_id.starts_with("evd_")||!safe_ref(actor_ref,128)
            ||request_id.is_some_and(|v|!safe_ref(v,128))||now_ms<=0 {
            return Err(PrivacyStoreError::InvalidInput);
        }
        let mut tx=begin_tenant_transaction(&self.pool, tenant).await?;
        let row=sqlx::query("SELECT evidence_id,resource_kind,resource_ref,data_class,status,retention_expires_at_ms,metadata,created_at_ms,deleted_at_ms FROM eye_evidence_references WHERE tenant_id=$1 AND evidence_id=$2 AND status='ACTIVE' FOR SHARE")
            .bind(tenant.as_str()).bind(evidence_id).fetch_optional(&mut *tx).await?
            .ok_or(PrivacyStoreError::NotFound)?;
        let evidence=EvidenceReference{
            evidence_id:row.try_get("evidence_id")?,resource_kind:row.try_get("resource_kind")?,
            resource_ref:row.try_get("resource_ref")?,data_class:row.try_get("data_class")?,
            status:row.try_get("status")?,retention_expires_at_ms:row.try_get("retention_expires_at_ms")?,
            metadata:row.try_get("metadata")?,created_at_ms:row.try_get("created_at_ms")?,
            deleted_at_ms:row.try_get("deleted_at_ms")?,
        };
        sqlx::query("INSERT INTO eye_audit_events(tenant_id,actor_kind,actor_ref,action,resource_kind,resource_ref,request_id,outcome,metadata,created_at_ms) VALUES($1,'API_CLIENT',$2,'EVIDENCE_ACCESSED','EVIDENCE',$3,$4,'SUCCESS',$5,$6)")
            .bind(tenant.as_str()).bind(actor_ref).bind(evidence_id).bind(request_id)
            .bind(serde_json::json!({"dataClass":evidence.data_class,"resourceKind":evidence.resource_kind}))
            .bind(now_ms).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(evidence)
    }

    pub async fn request_deletion(&self,tenant:&TenantId,target_kind:&str,target_ref:&str,
        reason_code:&str,actor_kind:&str,actor_ref:&str,key:&str,now_ms:i64)
        ->Result<DeletionRequest,PrivacyStoreError>{
        if !matches!(target_kind,"IDENTITY"|"ENROLLMENT"|"VERIFICATION_SESSION"|"EVIDENCE"|"FACE_TEMPLATE")
            ||!safe_ref(target_ref,128)||!matches!(reason_code,"SUBJECT_REQUEST"|"CONSENT_WITHDRAWN"|"RETENTION_EXPIRED"|"TENANT_REQUEST"|"SECURITY_REMEDIATION")
            ||!matches!(actor_kind,"USER"|"API_CLIENT"|"SYSTEM")||!safe_ref(actor_ref,128)||!safe_key(key)||now_ms<=0 {
            return Err(PrivacyStoreError::InvalidInput);
        }
        let mut tx=begin_tenant_transaction(&self.pool, tenant).await?;
        // Resolve idempotent replay before mutating evidence state.
        if let Some(existing)=sqlx::query("SELECT deletion_id,target_kind,target_ref,reason_code,actor_kind,actor_ref,status,attempts,max_attempts,lease_token,lease_until_ms FROM eye_deletion_requests WHERE tenant_id=$1 AND target_kind=$2 AND target_ref=$3 AND idempotency_key=$4 FOR UPDATE")
            .bind(tenant.as_str()).bind(target_kind).bind(target_ref).bind(key).fetch_optional(&mut *tx).await? {
            let same:String=existing.try_get("reason_code")?;
            let same_actor:String=existing.try_get("actor_kind")?;
            let same_ref:String=existing.try_get("actor_ref")?;
            if same!=reason_code||same_actor!=actor_kind||same_ref!=actor_ref {
                return Err(PrivacyStoreError::Conflict);
            }
            let request=map_deletion(existing)?;
            tx.commit().await?;
            return Ok(request);
        }
        let exists=match target_kind {
            "IDENTITY"=>sqlx::query_scalar::<_,String>("SELECT identity_id FROM eye_identities WHERE tenant_id=$1 AND identity_id=$2")
                .bind(tenant.as_str()).bind(target_ref).fetch_optional(&mut *tx).await?.is_some(),
            "ENROLLMENT"=>sqlx::query_scalar::<_,String>("SELECT enrollment_id FROM eye_identity_enrollments WHERE tenant_id=$1 AND enrollment_id=$2")
                .bind(tenant.as_str()).bind(target_ref).fetch_optional(&mut *tx).await?.is_some(),
            "VERIFICATION_SESSION"=>sqlx::query_scalar::<_,String>("SELECT session_id FROM eye_verification_sessions WHERE tenant_id=$1 AND session_id=$2")
                .bind(tenant.as_str()).bind(target_ref).fetch_optional(&mut *tx).await?.is_some(),
            "EVIDENCE"=>sqlx::query_scalar::<_,String>("SELECT evidence_id FROM eye_evidence_references WHERE tenant_id=$1 AND evidence_id=$2")
                .bind(tenant.as_str()).bind(target_ref).fetch_optional(&mut *tx).await?.is_some(),
            "FACE_TEMPLATE"=>sqlx::query_scalar::<_,String>("SELECT evidence_id FROM eye_evidence_references WHERE tenant_id=$1 AND evidence_id=$2 AND data_class='FACE_TEMPLATE'")
                .bind(tenant.as_str()).bind(target_ref).fetch_optional(&mut *tx).await?.is_some(),
            _=>false,
        };
        if !exists {return Err(PrivacyStoreError::NotFound);}
        if matches!(target_kind,"EVIDENCE"|"FACE_TEMPLATE") {
            sqlx::query("UPDATE eye_evidence_references SET status='DELETE_PENDING' WHERE tenant_id=$1 AND evidence_id=$2 AND status='ACTIVE'")
                .bind(tenant.as_str()).bind(target_ref).execute(&mut *tx).await?;
        }
        let id=format!("del_{}",Uuid::new_v4().simple());
        let row=sqlx::query("INSERT INTO eye_deletion_requests(deletion_id,tenant_id,target_kind,target_ref,reason_code,actor_kind,actor_ref,idempotency_key,status,next_attempt_at_ms,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'QUEUED',$9,$9,$9) RETURNING deletion_id,target_kind,target_ref,reason_code,status,attempts,max_attempts,lease_token,lease_until_ms")
            .bind(&id).bind(tenant.as_str()).bind(target_kind).bind(target_ref).bind(reason_code).bind(actor_kind).bind(actor_ref).bind(key).bind(now_ms)
            .fetch_one(&mut *tx).await?;
        let request=map_deletion(row)?;
        sqlx::query("INSERT INTO eye_audit_events(tenant_id,actor_kind,actor_ref,action,resource_kind,resource_ref,outcome,metadata,created_at_ms) VALUES($1,$2,$3,'DELETION_REQUESTED','DELETION_REQUEST',$4,'SUCCESS',$5,$6)")
            .bind(tenant.as_str()).bind(actor_kind).bind(actor_ref).bind(&request.deletion_id)
            .bind(serde_json::json!({"targetKind":target_kind,"targetRef":target_ref,"reasonCode":reason_code})).bind(now_ms)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(request)
    }

    /// Converts due evidence references into durable, idempotent deletion work.
    pub async fn queue_expired_evidence_deletions(&self,tenant:&TenantId,now_ms:i64,limit:i64)
        ->Result<u64,PrivacyStoreError>{
        if now_ms<=0||!(1..=MAX_BATCH).contains(&limit){return Err(PrivacyStoreError::InvalidInput);}
        let mut tx=begin_tenant_transaction(&self.pool, tenant).await?;
        let rows=sqlx::query("WITH due AS (SELECT evidence_id,tenant_id FROM eye_evidence_references WHERE tenant_id=$1 AND status='ACTIVE' AND retention_expires_at_ms<=$2 ORDER BY retention_expires_at_ms,created_at_ms LIMIT $3 FOR UPDATE SKIP LOCKED), marked AS (UPDATE eye_evidence_references e SET status='DELETE_PENDING' FROM due WHERE e.tenant_id=due.tenant_id AND e.evidence_id=due.evidence_id RETURNING e.tenant_id,e.evidence_id), queued AS (INSERT INTO eye_deletion_requests(deletion_id,tenant_id,target_kind,target_ref,reason_code,actor_kind,actor_ref,idempotency_key,status,next_attempt_at_ms,created_at_ms,updated_at_ms) SELECT 'del_'||md5(m.tenant_id||':'||m.evidence_id),m.tenant_id,'EVIDENCE',m.evidence_id,'RETENTION_EXPIRED','SYSTEM','retention-scheduler','retention-'||m.evidence_id,'QUEUED',$2,$2,$2 FROM marked m ON CONFLICT(tenant_id,target_kind,target_ref,idempotency_key) DO NOTHING RETURNING tenant_id,deletion_id,target_ref) INSERT INTO eye_audit_events(tenant_id,actor_kind,actor_ref,action,resource_kind,resource_ref,outcome,metadata,created_at_ms) SELECT tenant_id,'SYSTEM','retention-scheduler','RETENTION_DELETION_QUEUED','EVIDENCE',target_ref,'SUCCESS','{}'::jsonb,$2 FROM queued")
            .bind(tenant.as_str()).bind(now_ms).bind(limit).execute(&mut *tx).await?;
        let count=rows.rows_affected();
        tx.commit().await?;
        Ok(count)
    }

    /// Marks expired final-attempt leases as failed so cleanup work cannot remain stuck forever.
    pub async fn recover_exhausted_deletions(&self,tenant:&TenantId,now_ms:i64,limit:i64)
        ->Result<u64,PrivacyStoreError>{
        if now_ms<=0||!(1..=MAX_BATCH).contains(&limit){return Err(PrivacyStoreError::InvalidInput);}
        let mut tx=begin_tenant_transaction(&self.pool, tenant).await?;
        let rows=sqlx::query("WITH due AS (SELECT deletion_id,target_ref FROM eye_deletion_requests WHERE tenant_id=$1 AND status='PROCESSING' AND lease_until_ms<=$2 AND attempts>=max_attempts ORDER BY lease_until_ms LIMIT $3 FOR UPDATE SKIP LOCKED), failed AS (UPDATE eye_deletion_requests d SET status='FAILED',lease_token=NULL,worker_id=NULL,lease_until_ms=NULL,last_error_category='LEASE_EXHAUSTED',updated_at_ms=$2 FROM due WHERE d.deletion_id=due.deletion_id RETURNING d.deletion_id,d.target_ref) INSERT INTO eye_audit_events(tenant_id,actor_kind,actor_ref,action,resource_kind,resource_ref,outcome,metadata,created_at_ms) SELECT $1,'SYSTEM','deletion-recovery','DELETION_CLEANUP_FAILED','DELETION_REQUEST',deletion_id,'FAILED',jsonb_build_object('targetRef',target_ref,'reason','LEASE_EXHAUSTED'),$2 FROM failed")
            .bind(tenant.as_str()).bind(now_ms).bind(limit).execute(&mut *tx).await?;
        let count=rows.rows_affected();tx.commit().await?;Ok(count)
    }

    pub async fn claim_deletions(&self,tenant:&TenantId,worker_id:&str,now_ms:i64,lease_ms:i64,limit:i64)
        ->Result<Vec<ClaimedDeletion>,PrivacyStoreError>{
        if !safe_worker(worker_id)||now_ms<=0||!(1..=MAX_LEASE_MS).contains(&lease_ms)||!(1..=MAX_BATCH).contains(&limit){
            return Err(PrivacyStoreError::InvalidInput);
        }
        let mut tx=begin_tenant_transaction(&self.pool, tenant).await?;
        let rows=sqlx::query("WITH due AS (SELECT deletion_id FROM eye_deletion_requests WHERE tenant_id=$1 AND attempts<max_attempts AND ((status='QUEUED' AND next_attempt_at_ms<=$2) OR (status='PROCESSING' AND lease_until_ms<=$2)) ORDER BY next_attempt_at_ms,created_at_ms LIMIT $3 FOR UPDATE SKIP LOCKED), claimed AS (UPDATE eye_deletion_requests d SET status='PROCESSING',attempts=d.attempts+1,lease_token=gen_random_uuid(),worker_id=$4,lease_until_ms=$2+$5,updated_at_ms=$2 FROM due WHERE d.deletion_id=due.deletion_id RETURNING d.*) SELECT claimed.*,e.storage_key FROM claimed LEFT JOIN eye_evidence_references e ON e.tenant_id=claimed.tenant_id AND e.evidence_id=claimed.target_ref AND claimed.target_kind IN ('EVIDENCE','FACE_TEMPLATE') ORDER BY claimed.created_at_ms")
            .bind(tenant.as_str()).bind(now_ms).bind(limit).bind(worker_id).bind(lease_ms).fetch_all(&mut *tx).await?;
        let claimed=rows.into_iter().map(|row|{
            let storage_key:Option<String>=row.try_get("storage_key")?;
            let request=map_deletion(row)?;
            Ok(ClaimedDeletion{request,tenant_id:tenant.clone(),worker_id:worker_id.to_owned(),storage_key})
        }).collect::<Result<Vec<_>,PrivacyStoreError>>()?;
        tx.commit().await?;
        Ok(claimed)
    }

    pub async fn complete_deletion(&self,tenant:&TenantId,deletion_id:&str,lease_token:Uuid,worker_id:&str,now_ms:i64)
        ->Result<bool,PrivacyStoreError>{
        if !deletion_id.starts_with("del_")||!safe_worker(worker_id)||now_ms<=0{return Err(PrivacyStoreError::InvalidInput);}
        let mut tx=begin_tenant_transaction(&self.pool, tenant).await?;
        let row=sqlx::query("UPDATE eye_deletion_requests SET status='COMPLETED',lease_token=NULL,worker_id=NULL,lease_until_ms=NULL,completed_at_ms=$5,updated_at_ms=$5 WHERE tenant_id=$1 AND deletion_id=$2 AND status='PROCESSING' AND lease_token=$3 AND worker_id=$4 AND lease_until_ms>$5 RETURNING target_kind,target_ref")
            .bind(tenant.as_str()).bind(deletion_id).bind(lease_token).bind(worker_id).bind(now_ms).fetch_optional(&mut *tx).await?;
        let Some(row)=row else {tx.rollback().await?;return Ok(false)};
        let target_kind:String=row.try_get("target_kind")?;
        let target_ref:String=row.try_get("target_ref")?;
        if matches!(target_kind.as_str(),"EVIDENCE"|"FACE_TEMPLATE") {
            sqlx::query("UPDATE eye_evidence_references SET status='DELETED',deleted_at_ms=$3 WHERE tenant_id=$1 AND evidence_id=$2 AND status='DELETE_PENDING'")
                .bind(tenant.as_str()).bind(target_ref).bind(now_ms).execute(&mut *tx).await?;
        }
        sqlx::query("INSERT INTO eye_audit_events(tenant_id,actor_kind,actor_ref,action,resource_kind,resource_ref,outcome,metadata,created_at_ms) VALUES($1,'SYSTEM',$2,'DELETION_COMPLETED','DELETION_REQUEST',$3,'SUCCESS',$4,$5)")
            .bind(tenant.as_str()).bind(worker_id).bind(deletion_id).bind(serde_json::json!({"targetKind":target_kind,"targetRef":target_ref})).bind(now_ms).execute(&mut *tx).await?;
        tx.commit().await?;Ok(true)
    }

    pub async fn retry_deletion(&self,tenant:&TenantId,deletion_id:&str,lease_token:Uuid,worker_id:&str,
        error_category:&str,retry_delay_ms:i64,now_ms:i64)->Result<Option<String>,PrivacyStoreError>{
        if !deletion_id.starts_with("del_")||!safe_worker(worker_id)||!safe_ref(error_category,80)
            ||!error_category.bytes().all(|b|b.is_ascii_uppercase()||b.is_ascii_digit()||b==b'_')
            ||retry_delay_ms<0||now_ms<=0{return Err(PrivacyStoreError::InvalidInput);}
        let mut tx=begin_tenant_transaction(&self.pool, tenant).await?;
        let row=sqlx::query("UPDATE eye_deletion_requests SET status=CASE WHEN attempts>=max_attempts THEN 'FAILED' ELSE 'QUEUED' END,next_attempt_at_ms=$6+$7,last_error_category=$5,lease_token=NULL,worker_id=NULL,lease_until_ms=NULL,updated_at_ms=$6 WHERE tenant_id=$1 AND deletion_id=$2 AND status='PROCESSING' AND lease_token=$3 AND worker_id=$4 AND lease_until_ms>$6 RETURNING status")
            .bind(tenant.as_str()).bind(deletion_id).bind(lease_token).bind(worker_id).bind(error_category).bind(now_ms).bind(retry_delay_ms)
            .fetch_optional(&mut *tx).await?;
        let status=row.map(|r|r.get("status"));
        tx.commit().await?;
        Ok(status)
    }
}
fn map_deletion(row:sqlx::postgres::PgRow)->Result<DeletionRequest,PrivacyStoreError>{
    Ok(DeletionRequest{deletion_id:row.try_get("deletion_id")?,target_kind:row.try_get("target_kind")?,
        target_ref:row.try_get("target_ref")?,reason_code:row.try_get("reason_code")?,status:row.try_get("status")?,
        attempts:row.try_get("attempts")?,max_attempts:row.try_get("max_attempts")?,
        lease_token:row.try_get("lease_token")?,lease_until_ms:row.try_get("lease_until_ms")?})
}
