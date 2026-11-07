//! Tenant-scoped audit, evidence and retention persistence (#189).
//!
//! This module stores metadata and durable deletion work only. It never stores biometric
//! bytes, templates, signed URLs, secrets or provider payloads.
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;
use crate::persistence::tenants::TenantId;

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
                !["image","photo","biometric","embedding","template","token","secret","password","payload","signed_url","storage_key"]
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
}

#[derive(Clone)]
pub struct PrivacyStore{pool:PgPool}
impl PrivacyStore {
    pub fn new(pool:PgPool)->Self{Self{pool}}

    /// Replaces the active policy for one data class. No implicit/default retention is applied.
    pub async fn set_retention_policy(&self,tenant:&TenantId,data_class:&str,retention_ms:i64,
        version:&str,effective_at_ms:i64,now_ms:i64)->Result<RetentionPolicy,PrivacyStoreError>{
        if !matches!(data_class,"ORIGINAL_CAPTURE"|"FACE_TEMPLATE"|"EVIDENCE_METADATA"|"AUDIT_RECORD")
            ||!(1..=315_360_000_000).contains(&retention_ms)||!safe_ref(version,128)
            ||effective_at_ms<=0||now_ms<=0||effective_at_ms<now_ms {
            return Err(PrivacyStoreError::InvalidInput);
        }
        let mut tx=self.pool.begin().await?;
        sqlx::query("UPDATE eye_retention_policies SET active=FALSE WHERE tenant_id=$1 AND data_class=$2 AND active=TRUE")
            .bind(tenant.as_str()).bind(data_class).execute(&mut *tx).await?;
        let id=format!("ret_{}",Uuid::new_v4().simple());
        let row=sqlx::query("INSERT INTO eye_retention_policies(policy_id,tenant_id,data_class,retention_ms,policy_version,effective_at_ms,active,created_at_ms) VALUES($1,$2,$3,$4,$5,$6,TRUE,$7) RETURNING policy_id,data_class,retention_ms,policy_version,effective_at_ms")
            .bind(&id).bind(tenant.as_str()).bind(data_class).bind(retention_ms).bind(version).bind(effective_at_ms).bind(now_ms)
            .fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(RetentionPolicy{policy_id:row.try_get("policy_id")?,data_class:row.try_get("data_class")?,
            retention_ms:row.try_get("retention_ms")?,policy_version:row.try_get("policy_version")?,
            effective_at_ms:row.try_get("effective_at_ms")?})
    }

    pub async fn active_retention_policy(&self,tenant:&TenantId,data_class:&str,now_ms:i64)
        ->Result<RetentionPolicy,PrivacyStoreError>{
        if now_ms<=0||!matches!(data_class,"ORIGINAL_CAPTURE"|"FACE_TEMPLATE"|"EVIDENCE_METADATA"|"AUDIT_RECORD"){
            return Err(PrivacyStoreError::InvalidInput);
        }
        let row=sqlx::query("SELECT policy_id,data_class,retention_ms,policy_version,effective_at_ms FROM eye_retention_policies WHERE tenant_id=$1 AND data_class=$2 AND active=TRUE AND effective_at_ms<=$3")
            .bind(tenant.as_str()).bind(data_class).bind(now_ms).fetch_optional(&self.pool).await?
            .ok_or(PrivacyStoreError::NotFound)?;
        Ok(RetentionPolicy{policy_id:row.try_get("policy_id")?,data_class:row.try_get("data_class")?,
            retention_ms:row.try_get("retention_ms")?,policy_version:row.try_get("policy_version")?,
            effective_at_ms:row.try_get("effective_at_ms")?})
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
        let id=sqlx::query_scalar::<_,i64>("INSERT INTO eye_audit_events(tenant_id,actor_kind,actor_ref,action,resource_kind,resource_ref,request_id,outcome,metadata,created_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING audit_event_id")
            .bind(tenant.as_str()).bind(actor_kind).bind(actor_ref).bind(action).bind(resource_kind)
            .bind(resource_ref).bind(request_id).bind(outcome).bind(metadata).bind(now_ms)
            .fetch_one(&self.pool).await?;
        Ok(id)
    }

    pub async fn get_evidence(&self,tenant:&TenantId,evidence_id:&str)->Result<EvidenceReference,PrivacyStoreError>{
        if !evidence_id.starts_with("evd_"){return Err(PrivacyStoreError::InvalidInput);}
        let row=sqlx::query("SELECT evidence_id,resource_kind,resource_ref,data_class,status,retention_expires_at_ms,metadata,created_at_ms,deleted_at_ms FROM eye_evidence_references WHERE tenant_id=$1 AND evidence_id=$2")
            .bind(tenant.as_str()).bind(evidence_id).fetch_optional(&self.pool).await?
            .ok_or(PrivacyStoreError::NotFound)?;
        Ok(EvidenceReference{evidence_id:row.try_get("evidence_id")?,resource_kind:row.try_get("resource_kind")?,
            resource_ref:row.try_get("resource_ref")?,data_class:row.try_get("data_class")?,status:row.try_get("status")?,
            retention_expires_at_ms:row.try_get("retention_expires_at_ms")?,metadata:row.try_get("metadata")?,
            created_at_ms:row.try_get("created_at_ms")?,deleted_at_ms:row.try_get("deleted_at_ms")?})
    }

    pub async fn request_deletion(&self,tenant:&TenantId,target_kind:&str,target_ref:&str,
        reason_code:&str,actor_kind:&str,actor_ref:&str,key:&str,now_ms:i64)
        ->Result<DeletionRequest,PrivacyStoreError>{
        if !matches!(target_kind,"IDENTITY"|"ENROLLMENT"|"VERIFICATION_SESSION"|"EVIDENCE"|"FACE_TEMPLATE")
            ||!safe_ref(target_ref,128)||!matches!(reason_code,"SUBJECT_REQUEST"|"CONSENT_WITHDRAWN"|"RETENTION_EXPIRED"|"TENANT_REQUEST"|"SECURITY_REMEDIATION")
            ||!matches!(actor_kind,"USER"|"API_CLIENT"|"SYSTEM")||!safe_ref(actor_ref,128)||!safe_key(key)||now_ms<=0 {
            return Err(PrivacyStoreError::InvalidInput);
        }
        let mut tx=self.pool.begin().await?;
        if target_kind=="EVIDENCE" {
            let updated=sqlx::query("UPDATE eye_evidence_references SET status='DELETE_PENDING' WHERE tenant_id=$1 AND evidence_id=$2 AND status='ACTIVE'")
                .bind(tenant.as_str()).bind(target_ref).execute(&mut *tx).await?;
            if updated.rows_affected()==0 {
                let status=sqlx::query_scalar::<_,String>("SELECT status FROM eye_evidence_references WHERE tenant_id=$1 AND evidence_id=$2")
                    .bind(tenant.as_str()).bind(target_ref).fetch_optional(&mut *tx).await?;
                if status.is_none(){return Err(PrivacyStoreError::NotFound);}
            }
        }
        let id=format!("del_{}",Uuid::new_v4().simple());
        let row=sqlx::query("INSERT INTO eye_deletion_requests(deletion_id,tenant_id,target_kind,target_ref,reason_code,actor_kind,actor_ref,idempotency_key,status,next_attempt_at_ms,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,$7,$8,'QUEUED',$9,$9,$9) ON CONFLICT(tenant_id,target_kind,target_ref,idempotency_key) DO UPDATE SET idempotency_key=EXCLUDED.idempotency_key RETURNING deletion_id,target_kind,target_ref,reason_code,status,attempts,max_attempts,lease_token,lease_until_ms")
            .bind(&id).bind(tenant.as_str()).bind(target_kind).bind(target_ref).bind(reason_code).bind(actor_kind).bind(actor_ref).bind(key).bind(now_ms)
            .fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(map_deletion(row)?)
    }

    pub async fn claim_deletions(&self,tenant:&TenantId,worker_id:&str,now_ms:i64,lease_ms:i64,limit:i64)
        ->Result<Vec<ClaimedDeletion>,PrivacyStoreError>{
        if !safe_worker(worker_id)||now_ms<=0||!(1..=MAX_LEASE_MS).contains(&lease_ms)||!(1..=MAX_BATCH).contains(&limit){
            return Err(PrivacyStoreError::InvalidInput);
        }
        let rows=sqlx::query("WITH due AS (SELECT deletion_id FROM eye_deletion_requests WHERE tenant_id=$1 AND attempts<max_attempts AND ((status='QUEUED' AND next_attempt_at_ms<=$2) OR (status='PROCESSING' AND lease_until_ms<=$2)) ORDER BY next_attempt_at_ms,created_at_ms LIMIT $3 FOR UPDATE SKIP LOCKED), claimed AS (UPDATE eye_deletion_requests d SET status='PROCESSING',attempts=d.attempts+1,lease_token=gen_random_uuid(),worker_id=$4,lease_until_ms=$2+$5,updated_at_ms=$2 FROM due WHERE d.deletion_id=due.deletion_id RETURNING d.*) SELECT * FROM claimed ORDER BY created_at_ms")
            .bind(tenant.as_str()).bind(now_ms).bind(limit).bind(worker_id).bind(lease_ms).fetch_all(&self.pool).await?;
        rows.into_iter().map(|row|{
            let request=map_deletion(row.clone())?;
            Ok(ClaimedDeletion{request,tenant_id:tenant.clone(),worker_id:worker_id.to_owned()})
        }).collect()
    }

    pub async fn complete_deletion(&self,tenant:&TenantId,deletion_id:&str,lease_token:Uuid,worker_id:&str,now_ms:i64)
        ->Result<bool,PrivacyStoreError>{
        if !deletion_id.starts_with("del_")||!safe_worker(worker_id)||now_ms<=0{return Err(PrivacyStoreError::InvalidInput);}
        let mut tx=self.pool.begin().await?;
        let row=sqlx::query("UPDATE eye_deletion_requests SET status='COMPLETED',lease_token=NULL,worker_id=NULL,lease_until_ms=NULL,completed_at_ms=$5,updated_at_ms=$5 WHERE tenant_id=$1 AND deletion_id=$2 AND status='PROCESSING' AND lease_token=$3 AND worker_id=$4 AND lease_until_ms>$5 RETURNING target_kind,target_ref")
            .bind(tenant.as_str()).bind(deletion_id).bind(lease_token).bind(worker_id).bind(now_ms).fetch_optional(&mut *tx).await?;
        let Some(row)=row else {tx.rollback().await?;return Ok(false)};
        let target_kind:String=row.try_get("target_kind")?;
        let target_ref:String=row.try_get("target_ref")?;
        if target_kind=="EVIDENCE" {
            sqlx::query("UPDATE eye_evidence_references SET status='DELETED',deleted_at_ms=$3 WHERE tenant_id=$1 AND evidence_id=$2 AND status='DELETE_PENDING'")
                .bind(tenant.as_str()).bind(target_ref).bind(now_ms).execute(&mut *tx).await?;
        }
        tx.commit().await?;Ok(true)
    }

    pub async fn retry_deletion(&self,tenant:&TenantId,deletion_id:&str,lease_token:Uuid,worker_id:&str,
        error_category:&str,retry_delay_ms:i64,now_ms:i64)->Result<Option<String>,PrivacyStoreError>{
        if !deletion_id.starts_with("del_")||!safe_worker(worker_id)||!safe_ref(error_category,80)
            ||!error_category.bytes().all(|b|b.is_ascii_uppercase()||b.is_ascii_digit()||b==b'_')
            ||retry_delay_ms<0||now_ms<=0{return Err(PrivacyStoreError::InvalidInput);}
        let row=sqlx::query("UPDATE eye_deletion_requests SET status=CASE WHEN attempts>=max_attempts THEN 'FAILED' ELSE 'QUEUED' END,next_attempt_at_ms=$6+$7,last_error_category=$5,lease_token=NULL,worker_id=NULL,lease_until_ms=NULL,updated_at_ms=$6 WHERE tenant_id=$1 AND deletion_id=$2 AND status='PROCESSING' AND lease_token=$3 AND worker_id=$4 AND lease_until_ms>$6 RETURNING status")
            .bind(tenant.as_str()).bind(deletion_id).bind(lease_token).bind(worker_id).bind(error_category).bind(now_ms).bind(retry_delay_ms)
            .fetch_optional(&self.pool).await?;
        Ok(row.map(|r|r.get("status")))
    }
}
fn map_deletion(row:sqlx::postgres::PgRow)->Result<DeletionRequest,PrivacyStoreError>{
    Ok(DeletionRequest{deletion_id:row.try_get("deletion_id")?,target_kind:row.try_get("target_kind")?,
        target_ref:row.try_get("target_ref")?,reason_code:row.try_get("reason_code")?,status:row.try_get("status")?,
        attempts:row.try_get("attempts")?,max_attempts:row.try_get("max_attempts")?,
        lease_token:row.try_get("lease_token")?,lease_until_ms:row.try_get("lease_until_ms")?})
}
