//! Tenant-scoped durable jobs, leases and atomic result/outbox finalization (#187).
//!
//! This runtime persists execution state only. It does not perform inference, decide
//! consent, or turn a probabilistic result into business authorization. The legacy
//! eye_kafka_outbox (#113) remains a separate photo-ingress publisher.
use serde_json::Value;
use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::persistence::tenants::TenantId;

const MAX_BATCH: i64 = 100;
const MAX_LEASE_MS: i64 = 15 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum JobStoreError {
    #[error("job input is invalid")]
    InvalidInput,
    #[error("job was not found")]
    NotFound,
    #[error("job idempotency key conflicts with an existing request")]
    Conflict,
    #[error("job lease is no longer owned by this worker")]
    LeaseLost,
    #[error("job persistence is unavailable")]
    Unavailable,
}
impl From<sqlx::Error> for JobStoreError {
    fn from(_: sqlx::Error) -> Self { Self::Unavailable }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub job_id: String,
    pub tenant_id: TenantId,
    pub resource_type: String,
    pub resource_id: String,
    pub status: String,
    pub attempts: i32,
    pub max_attempts: i32,
    pub available_at_ms: i64,
    pub result: Option<Value>,
    pub error_category: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedJob {
    pub job: Job,
    pub lease_token: Uuid,
    pub worker_id: String,
    pub lease_until_ms: i64,
    pub input_metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnqueueOutcome { Created, Existing }

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedOutboxEvent {
    pub event_id: String,
    pub tenant_id: TenantId,
    pub job_id: String,
    pub event_type: String,
    pub payload: Value,
    pub attempts: i32,
    pub lease_token: Uuid,
    pub lease_until_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass { Retryable, Permanent }

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
fn valid_worker(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128
        && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'))
}
fn valid_error(value: &str) -> bool {
    !value.is_empty() && value.len() <= 80
        && value.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}
fn safe_object(value: &Value, max: usize) -> bool {
    value.is_object() && serde_json::to_vec(value).is_ok_and(|v| v.len() <= max)
}
fn map_job(row: sqlx::postgres::PgRow) -> Result<Job, JobStoreError> {
    let tenant: String = row.try_get("tenant_id")?;
    Ok(Job {
        job_id: row.try_get("job_id")?,
        tenant_id: TenantId::parse(tenant).map_err(|_| JobStoreError::Unavailable)?,
        resource_type: row.try_get("resource_type")?,
        resource_id: row.try_get("resource_id")?,
        status: row.try_get("status")?,
        attempts: row.try_get("attempts")?,
        max_attempts: row.try_get("max_attempts")?,
        available_at_ms: row.try_get("available_at_ms")?,
        result: row.try_get("result")?,
        error_category: row.try_get("error_category")?,
        created_at_ms: row.try_get("created_at_ms")?,
        updated_at_ms: row.try_get("updated_at_ms")?,
        completed_at_ms: row.try_get("completed_at_ms")?,
    })
}
const JOB_COLUMNS: &str = "job_id, tenant_id, resource_type, resource_id, status, attempts, max_attempts, available_at_ms, result, error_category, created_at_ms, updated_at_ms, completed_at_ms";

#[derive(Clone)]
pub struct JobStore { pool: PgPool }
impl JobStore {
    pub fn new(pool: PgPool) -> Self { Self { pool } }

    /// Idempotently persists a job. Identity enrollment jobs are accepted only when
    /// their aggregate is already QUEUED; BLOCKED_EVALUATION is never claimable.
    #[allow(clippy::too_many_arguments)]
    pub async fn enqueue(
        &self, tenant_id: &TenantId, resource_type: &str, resource_id: &str,
        idempotency_key: &str, input_metadata: Value, now_ms: i64, max_attempts: i32,
    ) -> Result<(EnqueueOutcome, Job), JobStoreError> {
        if !matches!(resource_type, "IDENTITY_ENROLLMENT" | "VERIFICATION_SESSION")
            || !valid_identifier(resource_id) || !valid_identifier(idempotency_key)
            || !safe_object(&input_metadata, 4096) || now_ms <= 0
            || !(1..=20).contains(&max_attempts) {
            return Err(JobStoreError::InvalidInput);
        }
        let mut tx = self.pool.begin().await?;
        // Resolve idempotent retries before checking mutable aggregate state.
        let existing = sqlx::query(&format!(
            "SELECT {JOB_COLUMNS} FROM eye_processing_jobs WHERE tenant_id=$1 AND resource_type=$2 AND resource_id=$3 AND idempotency_key=$4 FOR UPDATE"
        )).bind(tenant_id.as_str()).bind(resource_type).bind(resource_id)
         .bind(idempotency_key).fetch_optional(&mut *tx).await?;
        if let Some(row) = existing {
            let job = map_job(row)?;
            let old: Value = sqlx::query_scalar(
                "SELECT input_metadata FROM eye_processing_jobs WHERE job_id=$1"
            ).bind(&job.job_id).fetch_one(&mut *tx).await?;
            if old != input_metadata { return Err(JobStoreError::Conflict); }
            tx.commit().await?;
            return Ok((EnqueueOutcome::Existing, job));
        }
        // #186 has not supplied the authoritative verification-session aggregate.
        // Fail closed rather than enqueue arbitrary caller-selected session references.
        if resource_type != "IDENTITY_ENROLLMENT" { return Err(JobStoreError::NotFound); }
        let eligible = sqlx::query_scalar::<_, String>(
            "SELECT enrollment_id FROM eye_identity_enrollments WHERE tenant_id=$1 AND enrollment_id=$2 AND status='QUEUED' FOR UPDATE"
        ).bind(tenant_id.as_str()).bind(resource_id).fetch_optional(&mut *tx).await?.is_some();
        if !eligible { return Err(JobStoreError::NotFound); }
        let job_id = format!("job_{}", Uuid::new_v4().simple());
        let row = sqlx::query(&format!(
            "INSERT INTO eye_processing_jobs (job_id,tenant_id,resource_type,resource_id,idempotency_key,status,attempts,max_attempts,available_at_ms,input_metadata,created_at_ms,updated_at_ms) VALUES ($1,$2,$3,$4,$5,'QUEUED',0,$6,$7,$8,$7,$7) RETURNING {JOB_COLUMNS}"
        )).bind(&job_id).bind(tenant_id.as_str()).bind(resource_type).bind(resource_id)
         .bind(idempotency_key).bind(max_attempts).bind(now_ms).bind(input_metadata)
         .fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok((EnqueueOutcome::Created, map_job(row)?))
    }

    /// Claims only queued work and reclaims processing jobs whose lease has expired.
    /// PostgreSQL SKIP LOCKED gives concurrent workers disjoint claims.
    pub async fn claim(&self, worker_id: &str, now_ms: i64, lease_ms: i64, limit: i64)
        -> Result<Vec<ClaimedJob>, JobStoreError> {
        if !valid_worker(worker_id) || now_ms <= 0 || lease_ms <= 0
            || lease_ms > MAX_LEASE_MS || !(1..=MAX_BATCH).contains(&limit) {
            return Err(JobStoreError::InvalidInput);
        }
        let rows = sqlx::query(
            "WITH due AS (SELECT job_id FROM eye_processing_jobs WHERE (status='QUEUED' AND available_at_ms <= $1) OR (status='PROCESSING' AND lease_until_ms <= $1 AND attempts < max_attempts) ORDER BY available_at_ms,created_at_ms LIMIT $2 FOR UPDATE SKIP LOCKED), claimed AS (UPDATE eye_processing_jobs j SET status='PROCESSING',attempts=j.attempts+1,lease_token=gen_random_uuid(),worker_id=$3,lease_until_ms=$1+$4,heartbeat_at_ms=$1,started_at_ms=COALESCE(started_at_ms,$1),updated_at_ms=$1 FROM due WHERE j.job_id=due.job_id RETURNING j.*) SELECT c.*, c.lease_token, c.worker_id, c.lease_until_ms FROM claimed c ORDER BY c.created_at_ms"
        ).bind(now_ms).bind(limit).bind(worker_id).bind(lease_ms).fetch_all(&self.pool).await?;
        rows.into_iter().map(|row| {
            let lease_token: Uuid = row.try_get("lease_token")?;
            let worker_id: String = row.try_get("worker_id")?;
            let lease_until_ms: i64 = row.try_get("lease_until_ms")?;
            let input_metadata: Value = row.try_get("input_metadata")?;
            Ok(ClaimedJob { job: map_job(row)?, lease_token, worker_id, lease_until_ms, input_metadata })
        }).collect()
    }

    pub async fn heartbeat(&self, job_id: &str, lease_token: Uuid, worker_id: &str, now_ms: i64, lease_ms: i64)
        -> Result<i64, JobStoreError> {
        if !valid_identifier(job_id) || !valid_worker(worker_id) || now_ms <= 0 || lease_ms <= 0 || lease_ms > MAX_LEASE_MS {
            return Err(JobStoreError::InvalidInput);
        }
        let until = now_ms.saturating_add(lease_ms);
        let result = sqlx::query("UPDATE eye_processing_jobs SET heartbeat_at_ms=$4,lease_until_ms=$5,updated_at_ms=$4 WHERE job_id=$1 AND lease_token=$2 AND worker_id=$3 AND status='PROCESSING' AND lease_until_ms>$4")
            .bind(job_id).bind(lease_token).bind(worker_id).bind(now_ms).bind(until).execute(&self.pool).await?;
        if result.rows_affected()!=1 { return Err(JobStoreError::LeaseLost); }
        Ok(until)
    }

    /// Commits the terminal job state and its tenant-scoped notification atomically.
    pub async fn finalize(&self, job_id: &str, lease_token: Uuid, worker_id: &str,
        terminal_status: &str, result_value: Option<Value>, now_ms: i64)
        -> Result<bool, JobStoreError> {
        let event_type = match terminal_status {
            "COMPLETED" => "job.completed.v1", "FAILED" => "job.failed.v1",
            "EXPIRED" => "job.expired.v1", "CANCELLED" => "job.cancelled.v1",
            _ => return Err(JobStoreError::InvalidInput),
        };
        if !valid_identifier(job_id) || !valid_worker(worker_id) || now_ms <= 0
            || result_value.as_ref().is_some_and(|v| !safe_object(v,8192)) {
            return Err(JobStoreError::InvalidInput);
        }
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT tenant_id,resource_type,resource_id,status,lease_until_ms FROM eye_processing_jobs WHERE job_id=$1 AND lease_token=$2 AND worker_id=$3 FOR UPDATE")
            .bind(job_id).bind(lease_token).bind(worker_id).fetch_optional(&mut *tx).await?
            .ok_or(JobStoreError::LeaseLost)?;
        let status: String = row.try_get("status")?;
        let lease_until: Option<i64> = row.try_get("lease_until_ms")?;
        if status != "PROCESSING" || lease_until.is_none_or(|v| v <= now_ms) {
            return Err(JobStoreError::LeaseLost);
        }
        let tenant_id: String = row.try_get("tenant_id")?;
        let resource_type: String = row.try_get("resource_type")?;
        let resource_id: String = row.try_get("resource_id")?;
        sqlx::query("UPDATE eye_processing_jobs SET status=$2,result=$3,lease_token=NULL,worker_id=NULL,lease_until_ms=NULL,completed_at_ms=$4,updated_at_ms=$4 WHERE job_id=$1")
            .bind(job_id).bind(terminal_status).bind(&result_value).bind(now_ms).execute(&mut *tx).await?;
        let event_id = format!("evt_{}", Uuid::new_v4().simple());
        let payload = serde_json::json!({
            "eventId": event_id, "eventType": event_type, "eventVersion": "1.0.0",
            "jobId": job_id, "resourceType": resource_type, "resourceId": resource_id,
            "status": terminal_status, "result": result_value, "occurredAtMs": now_ms
        });
        sqlx::query("INSERT INTO eye_platform_outbox(event_id,tenant_id,job_id,event_type,payload,status,attempts,next_attempt_at_ms,created_at_ms,updated_at_ms) VALUES($1,$2,$3,$4,$5,'PENDING',0,$6,$6,$6)")
            .bind(event_id).bind(tenant_id).bind(job_id).bind(event_type).bind(payload).bind(now_ms)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Infrastructure failures can retry; domain-negative outcomes must be finalized
    /// as COMPLETED with a typed result, never represented as retryable failures.
    pub async fn fail(&self, job_id: &str, lease_token: Uuid, worker_id: &str,
        error_category: &str, class: FailureClass, now_ms: i64, retry_delay_ms: i64)
        -> Result<String, JobStoreError> {
        if !valid_identifier(job_id) || !valid_worker(worker_id) || !valid_error(error_category)
            || now_ms <= 0 || retry_delay_ms < 0 { return Err(JobStoreError::InvalidInput); }
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query("SELECT attempts,max_attempts,status,lease_until_ms FROM eye_processing_jobs WHERE job_id=$1 AND lease_token=$2 AND worker_id=$3 FOR UPDATE")
            .bind(job_id).bind(lease_token).bind(worker_id).fetch_optional(&mut *tx).await?
            .ok_or(JobStoreError::LeaseLost)?;
        let attempts: i32=row.try_get("attempts")?;
        let max_attempts: i32=row.try_get("max_attempts")?;
        let status: String=row.try_get("status")?;
        let lease_until: Option<i64>=row.try_get("lease_until_ms")?;
        if status!="PROCESSING" || lease_until.is_none_or(|v| v<=now_ms) { return Err(JobStoreError::LeaseLost); }
        let retry = class==FailureClass::Retryable && attempts<max_attempts;
        let next = if retry { "QUEUED" } else { "FAILED" };
        if retry {
            sqlx::query("UPDATE eye_processing_jobs SET status='QUEUED',available_at_ms=$4,error_category=$5,lease_token=NULL,worker_id=NULL,lease_until_ms=NULL,updated_at_ms=$6 WHERE job_id=$1 AND lease_token=$2 AND worker_id=$3")
                .bind(job_id).bind(lease_token).bind(worker_id).bind(now_ms.saturating_add(retry_delay_ms)).bind(error_category).bind(now_ms).execute(&mut *tx).await?;
        } else {
            sqlx::query("UPDATE eye_processing_jobs SET status='FAILED',error_category=$4,lease_token=NULL,worker_id=NULL,lease_until_ms=NULL,completed_at_ms=$5,updated_at_ms=$5 WHERE job_id=$1 AND lease_token=$2 AND worker_id=$3")
                .bind(job_id).bind(lease_token).bind(worker_id).bind(error_category).bind(now_ms).execute(&mut *tx).await?;
            let (tenant_id, resource_type, resource_id): (String,String,String)=sqlx::query_as("SELECT tenant_id,resource_type,resource_id FROM eye_processing_jobs WHERE job_id=$1").bind(job_id).fetch_one(&mut *tx).await?;
            let event_id=format!("evt_{}",Uuid::new_v4().simple());
            let payload=serde_json::json!({"eventId":event_id,"eventType":"job.failed.v1","eventVersion":"1.0.0","jobId":job_id,"resourceType":resource_type,"resourceId":resource_id,"status":"FAILED","errorCategory":error_category,"occurredAtMs":now_ms});
            sqlx::query("INSERT INTO eye_platform_outbox(event_id,tenant_id,job_id,event_type,payload,status,attempts,next_attempt_at_ms,created_at_ms,updated_at_ms) VALUES($1,$2,$3,'job.failed.v1',$4,'PENDING',0,$5,$5,$5)")
                .bind(event_id).bind(tenant_id).bind(job_id).bind(payload).bind(now_ms).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(next.to_owned())
    }


    /// Reaps crashed jobs that exhausted their attempt budget and emits failure atomically.
    pub async fn recover_exhausted_leases(&self, now_ms: i64, limit: i64) -> Result<u64, JobStoreError> {
        if now_ms <= 0 || !(1..=MAX_BATCH).contains(&limit) { return Err(JobStoreError::InvalidInput); }
        let mut tx = self.pool.begin().await?;
        let rows = sqlx::query("SELECT job_id,tenant_id,resource_type,resource_id FROM eye_processing_jobs WHERE status='PROCESSING' AND lease_until_ms <= $1 AND attempts >= max_attempts ORDER BY lease_until_ms LIMIT $2 FOR UPDATE SKIP LOCKED")
            .bind(now_ms).bind(limit).fetch_all(&mut *tx).await?;
        for row in &rows {
            let job_id: String = row.try_get("job_id")?;
            let tenant_id: String = row.try_get("tenant_id")?;
            let resource_type: String = row.try_get("resource_type")?;
            let resource_id: String = row.try_get("resource_id")?;
            sqlx::query("UPDATE eye_processing_jobs SET status='FAILED',error_category='LEASE_EXPIRED',lease_token=NULL,worker_id=NULL,lease_until_ms=NULL,completed_at_ms=$2,updated_at_ms=$2 WHERE job_id=$1")
                .bind(&job_id).bind(now_ms).execute(&mut *tx).await?;
            let event_id = format!("evt_{}", Uuid::new_v4().simple());
            let payload = serde_json::json!({"eventId":event_id,"eventType":"job.failed.v1","eventVersion":"1.0.0","jobId":job_id,"resourceType":resource_type,"resourceId":resource_id,"status":"FAILED","errorCategory":"LEASE_EXPIRED","occurredAtMs":now_ms});
            sqlx::query("INSERT INTO eye_platform_outbox(event_id,tenant_id,job_id,event_type,payload,status,attempts,next_attempt_at_ms,created_at_ms,updated_at_ms) VALUES($1,$2,$3,'job.failed.v1',$4,'PENDING',0,$5,$5,$5)")
                .bind(event_id).bind(tenant_id).bind(&job_id).bind(payload).bind(now_ms).execute(&mut *tx).await?;
        }
        let count = rows.len() as u64;
        tx.commit().await?;
        Ok(count)
    }

    /// Claims result events with a fencing token; delivery is at-least-once.
    pub async fn claim_outbox(&self, worker_id: &str, now_ms: i64, lease_ms: i64, limit: i64) -> Result<Vec<ClaimedOutboxEvent>, JobStoreError> {
        if !valid_worker(worker_id) || now_ms <= 0 || lease_ms <= 0 || lease_ms > MAX_LEASE_MS || !(1..=MAX_BATCH).contains(&limit) { return Err(JobStoreError::InvalidInput); }
        let rows = sqlx::query("WITH due AS (SELECT event_id FROM eye_platform_outbox WHERE (status='PENDING' AND next_attempt_at_ms <= $1) OR (status='IN_FLIGHT' AND lease_until_ms <= $1) ORDER BY next_attempt_at_ms,created_at_ms LIMIT $2 FOR UPDATE SKIP LOCKED), claimed AS (UPDATE eye_platform_outbox o SET status='IN_FLIGHT',attempts=o.attempts+1,lease_token=gen_random_uuid(),lease_until_ms=$1+$3,updated_at_ms=$1 FROM due WHERE o.event_id=due.event_id RETURNING o.event_id,o.tenant_id,o.job_id,o.event_type,o.payload,o.attempts,o.lease_token,o.lease_until_ms) SELECT * FROM claimed ORDER BY event_id")
            .bind(now_ms).bind(limit).bind(lease_ms).fetch_all(&self.pool).await?;
        rows.into_iter().map(|row| Ok(ClaimedOutboxEvent {
            event_id: row.try_get("event_id")?,
            tenant_id: TenantId::parse(row.try_get::<String,_>("tenant_id")?).map_err(|_| JobStoreError::Unavailable)?,
            job_id: row.try_get("job_id")?, event_type: row.try_get("event_type")?,
            payload: row.try_get("payload")?, attempts: row.try_get("attempts")?,
            lease_token: row.try_get("lease_token")?, lease_until_ms: row.try_get("lease_until_ms")?,
        })).collect()
    }

    pub async fn acknowledge_outbox(&self, event_id: &str, lease_token: Uuid, now_ms: i64) -> Result<bool, JobStoreError> {
        if !valid_identifier(event_id) || now_ms <= 0 { return Err(JobStoreError::InvalidInput); }
        let result = sqlx::query("UPDATE eye_platform_outbox SET status='DELIVERED',delivered_at_ms=$3,lease_token=NULL,lease_until_ms=NULL,last_error_category=NULL,updated_at_ms=$3 WHERE event_id=$1 AND lease_token=$2 AND status='IN_FLIGHT' AND lease_until_ms>$3")
            .bind(event_id).bind(lease_token).bind(now_ms).execute(&self.pool).await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn retry_outbox(&self, event_id: &str, lease_token: Uuid, error_category: &str, now_ms: i64, delay_ms: i64, max_attempts: i32) -> Result<bool, JobStoreError> {
        if !valid_identifier(event_id) || !valid_error(error_category) || now_ms <= 0 || delay_ms < 0 || !(1..=20).contains(&max_attempts) { return Err(JobStoreError::InvalidInput); }
        let result = sqlx::query("UPDATE eye_platform_outbox SET status=CASE WHEN attempts >= $5 THEN 'DEAD_LETTER' ELSE 'PENDING' END,next_attempt_at_ms=$3+$4,lease_token=NULL,lease_until_ms=NULL,last_error_category=$6,updated_at_ms=$3 WHERE event_id=$1 AND lease_token=$2 AND status='IN_FLIGHT' AND lease_until_ms>$3")
            .bind(event_id).bind(lease_token).bind(now_ms).bind(delay_ms).bind(max_attempts).bind(error_category).execute(&self.pool).await?;
        Ok(result.rows_affected() == 1)
    }

    /// Lease-safe cancellation. A processing worker cannot subsequently finalize.
    pub async fn cancel(&self, tenant_id: &TenantId, job_id: &str, now_ms: i64) -> Result<bool, JobStoreError> {
        if !valid_identifier(job_id) || now_ms<=0 { return Err(JobStoreError::InvalidInput); }
        let mut tx=self.pool.begin().await?;
        let row=sqlx::query("SELECT status,resource_type,resource_id FROM eye_processing_jobs WHERE tenant_id=$1 AND job_id=$2 FOR UPDATE")
            .bind(tenant_id.as_str()).bind(job_id).fetch_optional(&mut *tx).await?
            .ok_or(JobStoreError::NotFound)?;
        let status:String=row.try_get("status")?;
        if !matches!(status.as_str(),"QUEUED"|"PROCESSING") { tx.rollback().await?; return Ok(false); }
        let resource_type:String=row.try_get("resource_type")?;
        let resource_id:String=row.try_get("resource_id")?;
        sqlx::query("UPDATE eye_processing_jobs SET status='CANCELLED',lease_token=NULL,worker_id=NULL,lease_until_ms=NULL,completed_at_ms=$3,updated_at_ms=$3 WHERE tenant_id=$1 AND job_id=$2")
            .bind(tenant_id.as_str()).bind(job_id).bind(now_ms).execute(&mut *tx).await?;
        let event_id=format!("evt_{}",Uuid::new_v4().simple());
        let payload=serde_json::json!({"eventId":event_id,"eventType":"job.cancelled.v1","eventVersion":"1.0.0","jobId":job_id,"resourceType":resource_type,"resourceId":resource_id,"status":"CANCELLED","occurredAtMs":now_ms});
        sqlx::query("INSERT INTO eye_platform_outbox(event_id,tenant_id,job_id,event_type,payload,status,attempts,next_attempt_at_ms,created_at_ms,updated_at_ms) VALUES($1,$2,$3,'job.cancelled.v1',$4,'PENDING',0,$5,$5,$5)")
            .bind(event_id).bind(tenant_id.as_str()).bind(job_id).bind(payload).bind(now_ms).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(true)
    }

    pub async fn get(&self, tenant_id: &TenantId, job_id: &str) -> Result<Job, JobStoreError> {
        if !valid_identifier(job_id) { return Err(JobStoreError::InvalidInput); }
        let row=sqlx::query(&format!("SELECT {JOB_COLUMNS} FROM eye_processing_jobs WHERE tenant_id=$1 AND job_id=$2"))
            .bind(tenant_id.as_str()).bind(job_id).fetch_optional(&self.pool).await?
            .ok_or(JobStoreError::NotFound)?;
        map_job(row)
    }
}
