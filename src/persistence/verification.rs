//! Tenant-isolated identity scopes and verification-session persistence (#186).
//! This module stores references and lifecycle metadata only. It never stores captures,
//! templates, embeddings, similarity scores or business workflow state.
use serde_json::Value;
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;
use crate::persistence::tenants::TenantId;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum VerificationStoreError {
    #[error("verification input is invalid")] InvalidInput,
    #[error("verification resource was not found")] NotFound,
    #[error("verification idempotency key conflicts")] Conflict,
    #[error("verification persistence is unavailable")] Unavailable,
}
impl From<sqlx::Error> for VerificationStoreError {
    fn from(error: sqlx::Error) -> Self {
        if let sqlx::Error::Database(db)=&error {
            if db.code().as_deref()==Some("23505") { return Self::Conflict; }
        }
        Self::Unavailable
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityScope {
    pub scope_id:String, pub external_reference:String, pub display_name:String,
    pub description:Option<String>, pub status:String, pub member_count:i64,
    pub created_at_ms:i64, pub updated_at_ms:i64,
}
#[derive(Debug, Clone, PartialEq)]
pub struct VerificationSession {
    pub session_id:String, pub mode:String, pub identity_id:Option<String>,
    pub scope_id:Option<String>, pub status:String, pub outcome:Option<String>,
    pub reason_code:Option<String>, pub purpose:Option<String>, pub context:Value,
    pub expires_at_ms:i64, pub created_at_ms:i64, pub updated_at_ms:i64,
    pub completed_at_ms:Option<i64>,
}
fn valid_ref(v:&str)->bool {
    !v.is_empty() && v.len()<=128 && v.trim()==v
        && !v.chars().any(char::is_control)
}
fn valid_key(v:&str)->bool {
    !v.is_empty() && v.len()<=128
        && v.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'_'||b==b'-')
}
fn safe_object(v:&Value,max:usize)->bool {
    v.is_object() && serde_json::to_vec(v).is_ok_and(|x|x.len()<=max)
}
fn map_session(row:sqlx::postgres::PgRow)->Result<VerificationSession,VerificationStoreError>{
    Ok(VerificationSession {
        session_id:row.try_get("session_id")?, mode:row.try_get("mode")?,
        identity_id:row.try_get("identity_id")?, scope_id:row.try_get("scope_id")?,
        status:row.try_get("status")?, outcome:row.try_get("outcome")?,
        reason_code:row.try_get("reason_code")?, purpose:row.try_get("purpose")?,
        context:row.try_get("context")?, expires_at_ms:row.try_get("expires_at_ms")?,
        created_at_ms:row.try_get("created_at_ms")?, updated_at_ms:row.try_get("updated_at_ms")?,
        completed_at_ms:row.try_get("completed_at_ms")?,
    })
}
const SESSION_COLUMNS:&str="session_id,mode,identity_id,scope_id,status,outcome,reason_code,purpose,context,expires_at_ms,created_at_ms,updated_at_ms,completed_at_ms";

#[derive(Clone)]
pub struct VerificationStore { pool:PgPool }
impl VerificationStore {
    pub fn new(pool:PgPool)->Self{Self{pool}}

    pub async fn create_scope(&self,tenant:&TenantId,external_reference:&str,display_name:&str,
        description:Option<&str>,now:i64)->Result<IdentityScope,VerificationStoreError>{
        let name=display_name.trim();
        if !valid_ref(external_reference)||name.is_empty()||name.len()>120
            ||name.chars().any(char::is_control)||description.is_some_and(|v|v.len()>500||v.chars().any(char::is_control))||now<=0 {
            return Err(VerificationStoreError::InvalidInput);
        }
        let id=format!("scp_{}",Uuid::new_v4().simple());
        let row=sqlx::query("INSERT INTO eye_identity_scopes(scope_id,tenant_id,external_reference,display_name,description,created_at_ms,updated_at_ms) SELECT $1,t.tenant_id,$3,$4,$5,$6,$6 FROM eye_tenants t WHERE t.tenant_id=$2 AND t.status='ACTIVE' RETURNING scope_id,external_reference,display_name,description,status,created_at_ms,updated_at_ms")
            .bind(&id).bind(tenant.as_str()).bind(external_reference).bind(name).bind(description).bind(now)
            .fetch_optional(&self.pool).await?.ok_or(VerificationStoreError::NotFound)?;
        Ok(IdentityScope{scope_id:row.try_get("scope_id")?,external_reference:row.try_get("external_reference")?,
            display_name:row.try_get("display_name")?,description:row.try_get("description")?,status:row.try_get("status")?,
            member_count:0,created_at_ms:row.try_get("created_at_ms")?,updated_at_ms:row.try_get("updated_at_ms")?})
    }

    pub async fn add_member(&self,tenant:&TenantId,scope_id:&str,identity_id:&str,now:i64)->Result<(),VerificationStoreError>{
        if !scope_id.starts_with("scp_")||!identity_id.starts_with("idy_")||now<=0{return Err(VerificationStoreError::InvalidInput)}
        let mut tx=self.pool.begin().await?;
        let scope=sqlx::query_scalar::<_,String>("SELECT scope_id FROM eye_identity_scopes WHERE tenant_id=$1 AND scope_id=$2 AND status='ACTIVE' FOR UPDATE")
            .bind(tenant.as_str()).bind(scope_id).fetch_optional(&mut *tx).await?;
        if scope.is_none(){return Err(VerificationStoreError::NotFound)}
        let identity=sqlx::query_scalar::<_,String>("SELECT identity_id FROM eye_identities WHERE tenant_id=$1 AND identity_id=$2 AND status='ACTIVE' FOR UPDATE")
            .bind(tenant.as_str()).bind(identity_id).fetch_optional(&mut *tx).await?;
        if identity.is_none(){return Err(VerificationStoreError::NotFound)}
        sqlx::query("INSERT INTO eye_identity_scope_members(tenant_id,scope_id,identity_id,created_at_ms,removed_at_ms) VALUES($1,$2,$3,$4,NULL) ON CONFLICT(tenant_id,scope_id,identity_id) DO UPDATE SET removed_at_ms=NULL WHERE eye_identity_scope_members.removed_at_ms IS NOT NULL")
            .bind(tenant.as_str()).bind(scope_id).bind(identity_id).bind(now).execute(&mut *tx).await?;
        tx.commit().await?; Ok(())
    }

    pub async fn remove_member(&self,tenant:&TenantId,scope_id:&str,identity_id:&str,now:i64)->Result<(),VerificationStoreError>{
        if now<=0{return Err(VerificationStoreError::InvalidInput)}
        let r=sqlx::query("UPDATE eye_identity_scope_members SET removed_at_ms=$4 WHERE tenant_id=$1 AND scope_id=$2 AND identity_id=$3 AND removed_at_ms IS NULL AND EXISTS(SELECT 1 FROM eye_identity_scopes s WHERE s.tenant_id=$1 AND s.scope_id=$2 AND s.status='ACTIVE')")
            .bind(tenant.as_str()).bind(scope_id).bind(identity_id).bind(now).execute(&self.pool).await?;
        if r.rows_affected()!=1{return Err(VerificationStoreError::NotFound)} Ok(())
    }

    pub async fn get_scope(&self,tenant:&TenantId,scope_id:&str)->Result<IdentityScope,VerificationStoreError>{
        let row=sqlx::query("SELECT s.scope_id,s.external_reference,s.display_name,s.description,s.status,s.created_at_ms,s.updated_at_ms,(SELECT count(*) FROM eye_identity_scope_members m JOIN eye_identities i ON i.tenant_id=m.tenant_id AND i.identity_id=m.identity_id WHERE m.tenant_id=s.tenant_id AND m.scope_id=s.scope_id AND m.removed_at_ms IS NULL AND i.status='ACTIVE') AS member_count FROM eye_identity_scopes s WHERE s.tenant_id=$1 AND s.scope_id=$2 AND s.status='ACTIVE'")
            .bind(tenant.as_str()).bind(scope_id).fetch_optional(&self.pool).await?.ok_or(VerificationStoreError::NotFound)?;
        Ok(IdentityScope{scope_id:row.try_get("scope_id")?,external_reference:row.try_get("external_reference")?,display_name:row.try_get("display_name")?,description:row.try_get("description")?,status:row.try_get("status")?,member_count:row.try_get("member_count")?,created_at_ms:row.try_get("created_at_ms")?,updated_at_ms:row.try_get("updated_at_ms")?})
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_session(&self,tenant:&TenantId,client_id:&str,mode:&str,identity_id:Option<&str>,
        scope_id:Option<&str>,purpose:Option<&str>,context:Value,key:&str,now:i64,ttl_ms:i64)
        ->Result<(bool,VerificationSession),VerificationStoreError>{
        if !matches!(mode,"MATCH"|"IDENTIFY")||!valid_key(key)||!safe_object(&context,2048)||now<=0
            ||!(1_000..=3_600_000).contains(&ttl_ms)||purpose.is_some_and(|p|p.trim().is_empty()||p.len()>128||p.chars().any(char::is_control))
            ||(mode=="MATCH"&&(identity_id.is_none()||scope_id.is_some()))||(mode=="IDENTIFY"&&(scope_id.is_none()||identity_id.is_some())) {
            return Err(VerificationStoreError::InvalidInput);
        }
        let expires=now.checked_add(ttl_ms).ok_or(VerificationStoreError::InvalidInput)?;
        let mut tx=self.pool.begin().await?;
        // Serialize concurrent first submissions for the same tenant/client/key.
        let lock_key=format!("{}:{}:{}",tenant.as_str(),client_id,key);
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(lock_key).execute(&mut *tx).await?;
        let existing=sqlx::query(&format!("SELECT {SESSION_COLUMNS} FROM eye_verification_sessions WHERE tenant_id=$1 AND client_id=$2 AND idempotency_key=$3 FOR UPDATE"))
            .bind(tenant.as_str()).bind(client_id).bind(key).fetch_optional(&mut *tx).await?;
        if let Some(row)=existing{
            let session=map_session(row)?;
            let same=session.mode==mode&&session.identity_id.as_deref()==identity_id&&session.scope_id.as_deref()==scope_id
                &&session.purpose.as_deref()==purpose&&session.context==context;
            if !same{return Err(VerificationStoreError::Conflict)}
            tx.commit().await?;return Ok((false,session))
        }
        if mode=="MATCH"{
            let found=sqlx::query_scalar::<_,String>("SELECT identity_id FROM eye_identities WHERE tenant_id=$1 AND identity_id=$2 AND status='ACTIVE' FOR SHARE")
                .bind(tenant.as_str()).bind(identity_id.unwrap()).fetch_optional(&mut *tx).await?;
            if found.is_none(){return Err(VerificationStoreError::NotFound)}
        }else{
            let found=sqlx::query_scalar::<_,String>("SELECT scope_id FROM eye_identity_scopes WHERE tenant_id=$1 AND scope_id=$2 AND status='ACTIVE' FOR SHARE")
                .bind(tenant.as_str()).bind(scope_id.unwrap()).fetch_optional(&mut *tx).await?;
            if found.is_none(){return Err(VerificationStoreError::NotFound)}
        }
        let id=format!("vfy_{}",Uuid::new_v4().simple());
        let row=sqlx::query(&format!("INSERT INTO eye_verification_sessions(session_id,tenant_id,client_id,mode,identity_id,scope_id,status,reason_code,purpose,context,idempotency_key,expires_at_ms,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,$6,'BLOCKED_EVALUATION','EVALUATION_DATA_REQUIRED',$7,$8,$9,$10,$11,$11) RETURNING {SESSION_COLUMNS}"))
            .bind(&id).bind(tenant.as_str()).bind(client_id).bind(mode).bind(identity_id).bind(scope_id).bind(purpose).bind(context).bind(key).bind(expires).bind(now)
            .fetch_one(&mut *tx).await?;
        sqlx::query("INSERT INTO eye_verification_session_audit(tenant_id,session_id,client_id,action,created_at_ms) VALUES($1,$2,$3,'SESSION_CREATED',$4)")
            .bind(tenant.as_str()).bind(&id).bind(client_id).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;Ok((true,map_session(row)?))
    }

    pub async fn get_session(&self,tenant:&TenantId,session_id:&str,now:i64)->Result<VerificationSession,VerificationStoreError>{
        if now<=0{return Err(VerificationStoreError::InvalidInput)}
        let mut tx=self.pool.begin().await?;
        let row=sqlx::query(&format!("SELECT {SESSION_COLUMNS},client_id FROM eye_verification_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE"))
            .bind(tenant.as_str()).bind(session_id).fetch_optional(&mut *tx).await?.ok_or(VerificationStoreError::NotFound)?;
        let status:String=row.try_get("status")?;let expires:i64=row.try_get("expires_at_ms")?;
        if expires<=now&&matches!(status.as_str(),"AWAITING_CAPTURE"|"BLOCKED_EVALUATION"|"QUEUED"|"PROCESSING"){
            sqlx::query("UPDATE eye_verification_sessions SET status='EXPIRED',reason_code='SESSION_EXPIRED',updated_at_ms=$3 WHERE tenant_id=$1 AND session_id=$2")
                .bind(tenant.as_str()).bind(session_id).bind(now).execute(&mut *tx).await?;
            let client:String=row.try_get("client_id")?;
            sqlx::query("INSERT INTO eye_verification_session_audit(tenant_id,session_id,client_id,action,created_at_ms) VALUES($1,$2,$3,'SESSION_EXPIRED',$4)")
                .bind(tenant.as_str()).bind(session_id).bind(client).bind(now).execute(&mut *tx).await?;
        }
        let updated=sqlx::query(&format!("SELECT {SESSION_COLUMNS} FROM eye_verification_sessions WHERE tenant_id=$1 AND session_id=$2"))
            .bind(tenant.as_str()).bind(session_id).fetch_one(&mut *tx).await?;
        tx.commit().await?;map_session(updated)
    }

    pub async fn cancel_session(&self,tenant:&TenantId,session_id:&str,now:i64)->Result<VerificationSession,VerificationStoreError>{
        let mut tx=self.pool.begin().await?;
        let row=sqlx::query("SELECT client_id,status FROM eye_verification_sessions WHERE tenant_id=$1 AND session_id=$2 FOR UPDATE")
            .bind(tenant.as_str()).bind(session_id).fetch_optional(&mut *tx).await?.ok_or(VerificationStoreError::NotFound)?;
        let status:String=row.try_get("status")?;
        if !matches!(status.as_str(),"AWAITING_CAPTURE"|"BLOCKED_EVALUATION"){return Err(VerificationStoreError::Conflict)}
        let client:String=row.try_get("client_id")?;
        sqlx::query("UPDATE eye_verification_sessions SET status='CANCELLED',reason_code='SESSION_CANCELLED',updated_at_ms=$3 WHERE tenant_id=$1 AND session_id=$2")
            .bind(tenant.as_str()).bind(session_id).bind(now).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO eye_verification_session_audit(tenant_id,session_id,client_id,action,created_at_ms) VALUES($1,$2,$3,'SESSION_CANCELLED',$4)")
            .bind(tenant.as_str()).bind(session_id).bind(client).bind(now).execute(&mut *tx).await?;
        let result=sqlx::query(&format!("SELECT {SESSION_COLUMNS} FROM eye_verification_sessions WHERE tenant_id=$1 AND session_id=$2"))
            .bind(tenant.as_str()).bind(session_id).fetch_one(&mut *tx).await?;
        tx.commit().await?;map_session(result)
    }
}
