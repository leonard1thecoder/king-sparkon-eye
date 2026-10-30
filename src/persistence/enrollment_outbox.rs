//! SQLx-backed durable Eye enrollment metadata and transactional outbox (#113).
//!
//! This module owns only Eye's technical enrollment metadata and delivery intent.
//! It does not decide consent, ticket eligibility, worker authorization or admission.
//! Callers must verify authorization and authoritative consent before committing.
//! Object storage and PostgreSQL are not a distributed transaction: callers must
//! compensate an uploaded object when this transaction fails and run orphan reconciliation.

use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};
use thiserror::Error;

pub const ENROLLMENT_TOPIC: &str = "eye.enrollments.v1";
pub const MAX_ERROR_CATEGORY_LEN: usize = 80;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OutboxStoreError {
    #[error("durable enrollment store is unavailable")]
    Unavailable,
    #[error("enrollment idempotency conflict")]
    IdempotencyConflict,
    #[error("outbox operation was rejected")]
    Rejected,
}

impl From<sqlx::Error> for OutboxStoreError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}

#[derive(Debug, Clone)]
pub struct EnrollmentOutboxInput {
    pub enrollment_id: String,
    pub event_id: String,
    pub consent_reference: String,
    pub object_key: String,
    pub content_sha256: String,
    pub byte_len: i64,
    pub image_width: i32,
    pub image_height: i32,
    pub policy_version: String,
    pub model_version: Option<String>,
    pub idempotency_key: Option<String>,
    pub created_at_ms: i64,
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    Created,
    Existing,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaimedOutboxEvent {
    pub event_id: String,
    pub enrollment_id: String,
    pub consent_reference: String,
    pub policy_version: String,
    pub payload: Value,
    pub attempts: i32,
}

#[derive(Clone)]
pub struct EnrollmentOutboxStore {
    pool: PgPool,
}

impl EnrollmentOutboxStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Atomically writes Eye metadata and its Kafka delivery intent.
    /// The object must already exist in private object storage.
    pub async fn commit_enrollment(
        &self,
        input: &EnrollmentOutboxInput,
    ) -> Result<CommitOutcome, OutboxStoreError> {
        validate_input(input)?;
        let mut tx = self.pool.begin().await?;

        if let Some(key) = input.idempotency_key.as_deref() {
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(format!("eye-enrollment-idempotency:{key}"))
                .execute(&mut *tx)
                .await?;

            let existing = sqlx::query(
                "SELECT enrollment_id, event_id, consent_reference, content_sha256, policy_version                  FROM eye_enrollment_records r JOIN eye_kafka_outbox o USING (enrollment_id)                  WHERE r.idempotency_key = $1 FOR UPDATE OF r, o",
            )
            .bind(key)
            .fetch_optional(&mut *tx)
            .await?;

            if let Some(row) = existing {
                let same = row.try_get::<String, _>("content_sha256").ok().as_deref()
                    == Some(input.content_sha256.as_str())
                    && row
                        .try_get::<String, _>("consent_reference")
                        .ok()
                        .as_deref()
                        == Some(input.consent_reference.as_str())
                    && row.try_get::<String, _>("policy_version").ok().as_deref()
                        == Some(input.policy_version.as_str());
                let event_id = row.try_get::<String, _>("event_id").ok();
                let enrollment_id = row.try_get::<String, _>("enrollment_id").ok();
                if same
                    && event_id.as_deref() == Some(input.event_id.as_str())
                    && enrollment_id.as_deref() == Some(input.enrollment_id.as_str())
                {
                    tx.commit().await?;
                    return Ok(CommitOutcome::Existing);
                }
                return Err(OutboxStoreError::IdempotencyConflict);
            }
        }

        sqlx::query(
            "INSERT INTO eye_enrollment_records              (enrollment_id, consent_reference, object_key, content_sha256, byte_len, image_width,               image_height, policy_version, model_version, idempotency_key, status, persistence_status, created_at_ms)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'STORED','PENDING',$11)",
        )
        .bind(&input.enrollment_id)
        .bind(&input.consent_reference)
        .bind(&input.object_key)
        .bind(&input.content_sha256)
        .bind(input.byte_len)
        .bind(input.image_width)
        .bind(input.image_height)
        .bind(&input.policy_version)
        .bind(&input.model_version)
        .bind(&input.idempotency_key)
        .bind(input.created_at_ms)
        .execute(&mut *tx)
        .await?;

        sqlx::query(
            "INSERT INTO eye_kafka_outbox              (event_id, enrollment_id, topic, partition_key, payload, status, attempts,               next_attempt_at_ms, created_at_ms, updated_at_ms)              VALUES ($1,$2,$3,$2,$4,'PENDING',0,$5,$5,$5)",
        )
        .bind(&input.event_id)
        .bind(&input.enrollment_id)
        .bind(ENROLLMENT_TOPIC)
        .bind(&input.payload)
        .bind(input.created_at_ms)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(CommitOutcome::Created)
    }

    /// Atomically claims due work. Expired leases are reclaimed after process crashes.
    pub async fn claim_due(
        &self,
        now_ms: i64,
        lease_ms: i64,
        batch_size: i64,
    ) -> Result<Vec<ClaimedOutboxEvent>, OutboxStoreError> {
        if lease_ms <= 0 || batch_size <= 0 || batch_size > 100 {
            return Err(OutboxStoreError::Rejected);
        }
        let rows = sqlx::query(
            "WITH due AS (                 SELECT event_id FROM eye_kafka_outbox                 WHERE (status = 'PENDING' AND next_attempt_at_ms <= $1)                    OR (status = 'IN_FLIGHT' AND lease_until_ms <= $1)                 ORDER BY next_attempt_at_ms, created_at_ms                 LIMIT $2 FOR UPDATE SKIP LOCKED              ), claimed AS (                 UPDATE eye_kafka_outbox o SET status='IN_FLIGHT', lease_until_ms=$1+$3,                     attempts=o.attempts+1, updated_at_ms=$1                 FROM due WHERE o.event_id=due.event_id                 RETURNING o.event_id, o.enrollment_id, o.payload, o.attempts              )              SELECT c.event_id, c.enrollment_id, r.consent_reference, r.policy_version, c.payload, c.attempts              FROM claimed c JOIN eye_enrollment_records r USING (enrollment_id)              ORDER BY c.event_id",
        )
        .bind(now_ms)
        .bind(batch_size)
        .bind(lease_ms)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                Ok(ClaimedOutboxEvent {
                    event_id: row.try_get("event_id")?,
                    enrollment_id: row.try_get("enrollment_id")?,
                    consent_reference: row.try_get("consent_reference")?,
                    policy_version: row.try_get("policy_version")?,
                    payload: row.try_get("payload")?,
                    attempts: row.try_get("attempts")?,
                })
            })
            .collect()
    }

    /// A broker acknowledgement is the only transition to DELIVERED.
    pub async fn mark_delivered(
        &self,
        event_id: &str,
        now_ms: i64,
    ) -> Result<bool, OutboxStoreError> {
        let result = sqlx::query(
            "UPDATE eye_kafka_outbox SET status='DELIVERED', delivered_at_ms=$2,              lease_until_ms=NULL, last_error_category=NULL, updated_at_ms=$2              WHERE event_id=$1 AND status='IN_FLIGHT'",
        )
        .bind(event_id)
        .bind(now_ms)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Reschedules retryable failures or durably dead-letters after the attempt budget.
    pub async fn reschedule(
        &self,
        event_id: &str,
        error_category: &str,
        now_ms: i64,
        delay_ms: i64,
        max_attempts: i32,
    ) -> Result<bool, OutboxStoreError> {
        if !valid_error_category(error_category) || delay_ms < 0 || max_attempts < 1 {
            return Err(OutboxStoreError::Rejected);
        }
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT attempts FROM eye_kafka_outbox WHERE event_id=$1 AND status='IN_FLIGHT' FOR UPDATE",
        )
        .bind(event_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(false);
        };
        let attempts: i32 = row.try_get("attempts")?;
        if attempts >= max_attempts {
            dead_letter_in_tx(&mut tx, event_id, error_category, attempts, now_ms).await?;
        } else {
            sqlx::query(
                "UPDATE eye_kafka_outbox SET status='PENDING', next_attempt_at_ms=$2+$3,                  lease_until_ms=NULL, last_error_category=$4, updated_at_ms=$2                  WHERE event_id=$1 AND status='IN_FLIGHT'",
            )
            .bind(event_id)
            .bind(now_ms)
            .bind(delay_ms)
            .bind(error_category)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    pub async fn dead_letter(
        &self,
        event_id: &str,
        error_category: &str,
        now_ms: i64,
    ) -> Result<bool, OutboxStoreError> {
        if !valid_error_category(error_category) {
            return Err(OutboxStoreError::Rejected);
        }
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT attempts FROM eye_kafka_outbox WHERE event_id=$1 AND status <> 'DELIVERED' FOR UPDATE",
        )
        .bind(event_id)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(false);
        };
        let attempts: i32 = row.try_get("attempts")?;
        dead_letter_in_tx(&mut tx, event_id, error_category, attempts, now_ms).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Replay is an operator-only primitive. Authenticate and authorize the operator before calling.
    pub async fn replay_dead_letter(
        &self,
        event_id: &str,
        operator_reference: &str,
        now_ms: i64,
    ) -> Result<bool, OutboxStoreError> {
        if !valid_identifier(operator_reference) {
            return Err(OutboxStoreError::Rejected);
        }
        let mut tx = self.pool.begin().await?;
        let result = sqlx::query(
            "UPDATE eye_kafka_outbox SET status='PENDING', attempts=0, next_attempt_at_ms=$2, lease_until_ms=NULL, last_error_category=NULL, delivered_at_ms=NULL, updated_at_ms=$2 WHERE event_id=$1 AND status='DEAD_LETTER'              AND EXISTS (SELECT 1 FROM eye_kafka_dead_letters d WHERE d.event_id=$1)",
        )
        .bind(event_id)
        .bind(now_ms)
        .execute(&mut *tx)
        .await?;
        if result.rows_affected() == 1 {
            sqlx::query(
                "UPDATE eye_kafka_dead_letters SET replayed_at_ms=$2, replayed_by=$3 WHERE event_id=$1",
            )
            .bind(event_id)
            .bind(now_ms)
            .bind(operator_reference)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            return Ok(true);
        }
        tx.rollback().await?;
        Ok(false)
    }
}

async fn dead_letter_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    event_id: &str,
    error_category: &str,
    attempts: i32,
    now_ms: i64,
) -> Result<(), OutboxStoreError> {
    sqlx::query(
        "UPDATE eye_kafka_outbox SET status='DEAD_LETTER', lease_until_ms=NULL,          last_error_category=$2, updated_at_ms=$3 WHERE event_id=$1 AND status <> 'DELIVERED'",
    )
    .bind(event_id)
    .bind(error_category)
    .bind(now_ms)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO eye_kafka_dead_letters(event_id, error_category, attempts, dead_lettered_at_ms)          VALUES ($1,$2,$3,$4) ON CONFLICT (event_id) DO UPDATE SET error_category=EXCLUDED.error_category,          attempts=EXCLUDED.attempts, dead_lettered_at_ms=EXCLUDED.dead_lettered_at_ms,          replayed_at_ms=NULL, replayed_by=NULL",
    )
    .bind(event_id)
    .bind(error_category)
    .bind(attempts)
    .bind(now_ms)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn validate_input(input: &EnrollmentOutboxInput) -> Result<(), OutboxStoreError> {
    if !valid_identifier(&input.enrollment_id)
        || !valid_identifier(&input.event_id)
        || !valid_consent_reference(&input.consent_reference)
        || !input.object_key.starts_with("enrollments/")
        || input.object_key.len() <= "enrollments/".len()
        || input.object_key.len() > 512
        || input.object_key.contains("..")
        || input.object_key.contains("://")
        || input.object_key.contains('\\')
        || input.content_sha256.len() != 64
        || !input
            .content_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || input.byte_len <= 0
        || input.image_width <= 0
        || input.image_height <= 0
        || !valid_version(&input.policy_version)
        || input
            .model_version
            .as_deref()
            .is_some_and(|v| !valid_version(v))
        || input
            .idempotency_key
            .as_deref()
            .is_some_and(|v| !valid_identifier(v))
        || input.created_at_ms <= 0
        || !payload_matches(input)
    {
        return Err(OutboxStoreError::Rejected);
    }
    Ok(())
}

fn payload_matches(input: &EnrollmentOutboxInput) -> bool {
    let Some(object) = input.payload.as_object() else {
        return false;
    };
    const ALLOWED_KEYS: &[&str] = &[
        "eventId",
        "eventType",
        "eventVersion",
        "enrollmentId",
        "objectKey",
        "contentSha256",
        "byteLen",
        "dimensions",
        "consentReference",
        "policyVersion",
        "modelVersion",
        "idempotencyKey",
        "persistence",
        "occurredAtMs",
    ];
    if object
        .keys()
        .any(|key| !ALLOWED_KEYS.contains(&key.as_str()))
    {
        return false;
    }
    let dimensions = input.payload.get("dimensions");
    input.payload.get("eventId").and_then(Value::as_str) == Some(input.event_id.as_str())
        && input.payload.get("eventType").and_then(Value::as_str) == Some("photo.enrolled.v1")
        && input.payload.get("eventVersion").and_then(Value::as_str) == Some("1.0.0")
        && input.payload.get("enrollmentId").and_then(Value::as_str)
            == Some(input.enrollment_id.as_str())
        && input
            .payload
            .get("consentReference")
            .and_then(Value::as_str)
            == Some(input.consent_reference.as_str())
        && input.payload.get("policyVersion").and_then(Value::as_str)
            == Some(input.policy_version.as_str())
        && input.payload.get("persistence").and_then(Value::as_str) == Some("PENDING")
        && input.payload.get("objectKey").and_then(Value::as_str) == Some(input.object_key.as_str())
        && input.payload.get("contentSha256").and_then(Value::as_str)
            == Some(input.content_sha256.as_str())
        && input.payload.get("byteLen").and_then(Value::as_i64) == Some(input.byte_len)
        && dimensions
            .and_then(|value| value.get("width"))
            .and_then(Value::as_u64)
            == Some(input.image_width as u64)
        && dimensions
            .and_then(|value| value.get("height"))
            .and_then(Value::as_u64)
            == Some(input.image_height as u64)
        && input.payload.get("modelVersion").and_then(Value::as_str)
            == input.model_version.as_deref()
        && input.payload.get("idempotencyKey").and_then(Value::as_str)
            == input.idempotency_key.as_deref()
        && input.payload.get("occurredAtMs").and_then(Value::as_i64) == Some(input.created_at_ms)
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_consent_reference(value: &str) -> bool {
    (8..=128).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn valid_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
}

fn valid_error_category(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ERROR_CATEGORY_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}
