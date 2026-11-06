//! PostgreSQL-backed append-only usage ledger and transactional reporting outbox (#148).
//!
//! This module persists the semantics defined by `billing::ledger`. It stores only opaque
//! tenant/event references, meter identifiers and quantities. It never stores biometric material,
//! attendee data, ticket identifiers or raw source payloads. Callers remain responsible for
//! constructing requests only from authoritative billable boundaries agreed in #138.

use chrono::{DateTime, Duration, NaiveDate, NaiveTime, Utc};
use sqlx::{PgPool, Row};
use thiserror::Error;

use crate::billing::{
    AllowancePeriod, MeterId,
    ledger::{
        AppendBody, AppendOutcome, AppendRequest, BYTES_PER_GIB, TenantId, UsageSource,
    },
};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UsageStoreError {
    #[error("durable usage ledger is unavailable")]
    Unavailable,
    #[error("usage entry is invalid")]
    Invalid,
    #[error("usage event key conflicts with an existing entry")]
    KeyConflict,
    #[error("reversal target does not exist for this tenant")]
    ReversalTargetMissing,
    #[error("reversal target is not a reversible flow entry")]
    ReversalNotPermitted,
    #[error("usage entry has already been reversed")]
    AlreadyReversed,
    #[error("usage quantity arithmetic overflowed")]
    ArithmeticOverflow,
    #[error("a daily storage sample is missing")]
    MissingLevelSample,
    #[error("usage measurement window is invalid")]
    InvalidWindow,
}

impl From<sqlx::Error> for UsageStoreError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedUsageReport {
    pub id: i64,
    pub tenant_id: String,
    pub meter: MeterId,
    pub period_start: NaiveDate,
    pub attempts: i32,
}

#[derive(Clone)]
pub struct UsageLedgerStore {
    pool: PgPool,
}

impl UsageLedgerStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Append one usage occurrence and its reporting intent in the same PostgreSQL transaction.
    /// An identical replay returns the original sequence; reusing a key for different content
    /// fails with KeyConflict. Reversals are booked to the original flow's period.
    pub async fn append(
        &self,
        request: AppendRequest,
    ) -> Result<AppendOutcome, UsageStoreError> {
        let (body_kind, quantity, reversal_key) = encode_body(&request.body)?;
        validate_source_body(request.source, request.occurred_at, &request.body)?;
        let meter = meter_name(request.source.meter());
        let source = source_name(request.source);
        let mut tx = self.pool.begin().await?;

        let period_start = if let AppendBody::Reversal { reverses } = &request.body {
            let original = sqlx::query(
                "SELECT source, meter, body_kind, period_start FROM eye_billing_usage_ledger                  WHERE tenant_id = $1 AND event_key = $2 FOR UPDATE",
            )
            .bind(request.tenant.as_str())
            .bind(reverses.as_str())
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(UsageStoreError::ReversalTargetMissing)?;

            let original_source: String = original.try_get("source")?;
            let original_meter: String = original.try_get("meter")?;
            let original_kind: String = original.try_get("body_kind")?;
            if original_source != source || original_meter != meter || original_kind != "flow" {
                return Err(UsageStoreError::ReversalNotPermitted);
            }

            original.try_get("period_start")?
        } else {
            AllowancePeriod::containing(request.occurred_at).start_date()
        };

        // Fast path for an already committed event key, including a reversal retry.
        // The unique constraint below remains the concurrency-safe final guard.
        let existing = sqlx::query(
            "SELECT id, source, meter, occurred_at, period_start, body_kind, quantity, reversal_of_event_key              FROM eye_billing_usage_ledger WHERE tenant_id = $1 AND event_key = $2",
        )
        .bind(request.tenant.as_str())
        .bind(request.key.as_str())
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(existing) = existing {
            let same = existing.try_get::<String, _>("source")? == source
                && existing.try_get::<String, _>("meter")? == meter
                && existing.try_get::<DateTime<Utc>, _>("occurred_at")? == request.occurred_at
                && existing.try_get::<NaiveDate, _>("period_start")? == period_start
                && existing.try_get::<String, _>("body_kind")? == body_kind
                && existing.try_get::<Option<i64>, _>("quantity")? == quantity
                && existing.try_get::<Option<String>, _>("reversal_of_event_key")? == reversal_key;
            if !same {
                return Err(UsageStoreError::KeyConflict);
            }
            let id: i64 = existing.try_get("id")?;
            tx.commit().await?;
            return Ok(AppendOutcome::Duplicate {
                seq: u64::try_from(id).map_err(|_| UsageStoreError::ArithmeticOverflow)?,
            });
        }

        // Check for an identical replay before enforcing the one-reversal-per-target rule.
        // This makes retries of a committed reversal idempotent.
        if let AppendBody::Reversal { reverses } = &request.body {
            let already_reversed: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM eye_billing_usage_ledger WHERE tenant_id = $1 AND reversal_of_event_key = $2 AND body_kind = 'reversal')",
            )
            .bind(request.tenant.as_str())
            .bind(reverses.as_str())
            .fetch_one(&mut *tx)
            .await?;
            if already_reversed {
                return Err(UsageStoreError::AlreadyReversed);
            }
        }

        let inserted: Option<i64> = sqlx::query_scalar(
            "INSERT INTO eye_billing_usage_ledger              (tenant_id, event_key, source, meter, occurred_at, period_start, body_kind, quantity, reversal_of_event_key)              VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9)              ON CONFLICT (tenant_id, event_key) DO NOTHING RETURNING id",
        )
        .bind(request.tenant.as_str())
        .bind(request.key.as_str())
        .bind(source)
        .bind(meter)
        .bind(request.occurred_at)
        .bind(period_start)
        .bind(body_kind)
        .bind(quantity)
        .bind(reversal_key.as_deref())
        .fetch_optional(&mut *tx)
        .await?;

        let id = if let Some(id) = inserted {
            id
        } else {
            let existing = sqlx::query(
                "SELECT id, source, meter, occurred_at, period_start, body_kind, quantity, reversal_of_event_key                  FROM eye_billing_usage_ledger WHERE tenant_id = $1 AND event_key = $2",
            )
            .bind(request.tenant.as_str())
            .bind(request.key.as_str())
            .fetch_one(&mut *tx)
            .await?;

            let same = existing.try_get::<String, _>("source")? == source
                && existing.try_get::<String, _>("meter")? == meter
                && existing.try_get::<DateTime<Utc>, _>("occurred_at")? == request.occurred_at
                && existing.try_get::<NaiveDate, _>("period_start")? == period_start
                && existing.try_get::<String, _>("body_kind")? == body_kind
                && existing.try_get::<Option<i64>, _>("quantity")? == quantity
                && existing.try_get::<Option<String>, _>("reversal_of_event_key")?
                    == reversal_key;
            if !same {
                return Err(UsageStoreError::KeyConflict);
            }
            let id: i64 = existing.try_get("id")?;
            tx.commit().await?;
            return Ok(AppendOutcome::Duplicate {
                seq: u64::try_from(id).map_err(|_| UsageStoreError::ArithmeticOverflow)?,
            });
        };

        sqlx::query(
            "INSERT INTO eye_billing_usage_outbox (ledger_entry_id, tenant_id, meter, period_start)              VALUES ($1,$2,$3,$4) ON CONFLICT (ledger_entry_id) DO NOTHING",
        )
        .bind(id)
        .bind(request.tenant.as_str())
        .bind(meter)
        .bind(period_start)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(AppendOutcome::Appended {
            seq: u64::try_from(id).map_err(|_| UsageStoreError::ArithmeticOverflow)?,
        })
    }

    /// Net flow usage for one tenant/meter/period, subtracting explicit reversals.
    pub async fn flow_total(
        &self,
        tenant_id: &str,
        period: &AllowancePeriod,
        meter: MeterId,
    ) -> Result<u64, UsageStoreError> {
        let tenant = TenantId::new(tenant_id).map_err(|_| UsageStoreError::Invalid)?;
        if !matches!(meter, MeterId::Enrollments | MeterId::IdentificationRequests) {
            return Err(UsageStoreError::Invalid);
        }
        let total: String = sqlx::query_scalar(
            "SELECT COALESCE(SUM(                  CASE WHEN entry.body_kind = 'flow' THEN entry.quantity::numeric                       WHEN entry.body_kind = 'reversal' THEN -original.quantity::numeric                       ELSE 0 END), 0)::text              FROM eye_billing_usage_ledger entry              LEFT JOIN eye_billing_usage_ledger original                ON original.tenant_id = entry.tenant_id               AND original.event_key = entry.reversal_of_event_key              WHERE entry.tenant_id = $1 AND entry.period_start = $2                AND entry.meter = $3 AND entry.body_kind IN ('flow','reversal')",
        )
        .bind(tenant.as_str())
        .bind(period.start_date())
        .bind(meter_name(meter))
        .fetch_one(&self.pool)
        .await?;
        total.parse::<u64>().map_err(|_| UsageStoreError::ArithmeticOverflow)
    }

    /// Claim due reporting-intent rows with crash-safe leases and SKIP LOCKED concurrency.
    pub async fn claim_due(
        &self,
        now: DateTime<Utc>,
        lease: Duration,
        batch_size: i64,
    ) -> Result<Vec<ClaimedUsageReport>, UsageStoreError> {
        if lease <= Duration::zero() || !(1..=100).contains(&batch_size) {
            return Err(UsageStoreError::Invalid);
        }
        let lease_until = now
            .checked_add_signed(lease)
            .ok_or(UsageStoreError::ArithmeticOverflow)?;
        let rows = sqlx::query(
            "WITH due AS (                  SELECT id FROM eye_billing_usage_outbox                  WHERE (status = 'PENDING' AND next_attempt_at <= $1)                     OR (status = 'IN_FLIGHT' AND lease_until <= $1)                  ORDER BY next_attempt_at, id LIMIT $2 FOR UPDATE SKIP LOCKED              ), claimed AS (                  UPDATE eye_billing_usage_outbox outbox                  SET status = 'IN_FLIGHT', attempts = outbox.attempts + 1,                      lease_until = $3, updated_at = $1                  FROM due WHERE outbox.id = due.id                  RETURNING outbox.id, outbox.tenant_id, outbox.meter,                            outbox.period_start, outbox.attempts              )              SELECT id, tenant_id, meter, period_start, attempts FROM claimed ORDER BY id",
        )
        .bind(now)
        .bind(batch_size)
        .bind(lease_until)
        .fetch_all(&self.pool)
        .await?;

        rows.into_iter()
            .map(|row| {
                let meter: String = row.try_get("meter")?;
                Ok(ClaimedUsageReport {
                    id: row.try_get("id")?,
                    tenant_id: row.try_get("tenant_id")?,
                    meter: parse_meter(&meter).ok_or(UsageStoreError::Invalid)?,
                    period_start: row.try_get("period_start")?,
                    attempts: row.try_get("attempts")?,
                })
            })
            .collect()
    }

    /// Mark a claimed report complete only for the exact lease attempt that was claimed.
    pub async fn mark_reported(
        &self,
        id: i64,
        attempts: i32,
        reported_at: DateTime<Utc>,
    ) -> Result<bool, UsageStoreError> {
        let result = sqlx::query(
            "UPDATE eye_billing_usage_outbox              SET status = 'REPORTED', lease_until = NULL, updated_at = $3              WHERE id = $1 AND attempts = $2 AND status = 'IN_FLIGHT'",
        )
        .bind(id)
        .bind(attempts)
        .bind(reported_at)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Retry a report or retain it as a dead letter after the configured attempt limit.
    pub async fn schedule_retry(
        &self,
        id: i64,
        attempts: i32,
        now: DateTime<Utc>,
        next_attempt_at: DateTime<Utc>,
        error_category: &str,
        max_attempts: i32,
    ) -> Result<bool, UsageStoreError> {
        if max_attempts <= 0
            || next_attempt_at < now
            || error_category.is_empty()
            || error_category.len() > 80
            || !error_category
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(UsageStoreError::Invalid);
        }
        let status = if attempts >= max_attempts {
            "DEAD_LETTER"
        } else {
            "PENDING"
        };
        let result = sqlx::query(
            "UPDATE eye_billing_usage_outbox              SET status = $4, next_attempt_at = $5, lease_until = NULL,                  last_error_category = $6, updated_at = $3              WHERE id = $1 AND attempts = $2 AND status = 'IN_FLIGHT'",
        )
        .bind(id)
        .bind(attempts)
        .bind(now)
        .bind(status)
        .bind(next_attempt_at)
        .bind(error_category)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    /// Daily-level meter quantity: latest sample per UTC day, averaged across the inclusive
    /// billable window and rounded up. Every day must have a sample to avoid silent underbilling.
    pub async fn level_billable_quantity(
        &self,
        tenant_id: &str,
        period: &AllowancePeriod,
        meter: MeterId,
        first_day: NaiveDate,
        last_day: NaiveDate,
    ) -> Result<u64, UsageStoreError> {
        let tenant = TenantId::new(tenant_id).map_err(|_| UsageStoreError::Invalid)?;
        if first_day > last_day
            || first_day < period.start_date()
            || last_day >= period.end_exclusive_date()
            || !matches!(meter, MeterId::ActiveTemplates | MeterId::RetainedPhotoStorageGb)
        {
            return Err(UsageStoreError::InvalidWindow);
        }

        let rows = sqlx::query(
            "SELECT DISTINCT ON ((occurred_at AT TIME ZONE 'UTC')::date)                     (occurred_at AT TIME ZONE 'UTC')::date AS sample_day, quantity              FROM eye_billing_usage_ledger              WHERE tenant_id = $1 AND period_start = $2 AND meter = $3 AND body_kind = 'level'                AND (occurred_at AT TIME ZONE 'UTC')::date BETWEEN $4 AND $5              ORDER BY (occurred_at AT TIME ZONE 'UTC')::date, occurred_at DESC, id DESC",
        )
        .bind(tenant.as_str())
        .bind(period.start_date())
        .bind(meter_name(meter))
        .bind(first_day)
        .bind(last_day)
        .fetch_all(&self.pool)
        .await?;

        let mut samples = std::collections::BTreeMap::new();
        for row in rows {
            let day: NaiveDate = row.try_get("sample_day")?;
            let quantity: i64 = row.try_get("quantity")?;
            let quantity = u64::try_from(quantity)
                .map_err(|_| UsageStoreError::ArithmeticOverflow)?;
            samples.insert(day, quantity);
        }

        let days = (last_day - first_day).num_days() + 1;
        let days = u64::try_from(days).map_err(|_| UsageStoreError::InvalidWindow)?;
        let mut sum = 0_u128;
        let mut day = first_day;
        loop {
            let value = samples
                .get(&day)
                .copied()
                .ok_or(UsageStoreError::MissingLevelSample)?;
            sum = sum
                .checked_add(u128::from(value))
                .ok_or(UsageStoreError::ArithmeticOverflow)?;
            if day == last_day {
                break;
            }
            day = day
                .checked_add_signed(Duration::days(1))
                .ok_or(UsageStoreError::InvalidWindow)?;
        }

        let divisor = match meter {
            MeterId::ActiveTemplates => u128::from(days),
            MeterId::RetainedPhotoStorageGb => u128::from(days)
                .checked_mul(BYTES_PER_GIB)
                .ok_or(UsageStoreError::ArithmeticOverflow)?,
            _ => return Err(UsageStoreError::InvalidWindow),
        };
        let quantity = sum.div_ceil(divisor);
        u64::try_from(quantity).map_err(|_| UsageStoreError::ArithmeticOverflow)
    }
}

fn encode_body(body: &AppendBody) -> Result<(&'static str, Option<i64>, Option<String>), UsageStoreError> {
    match body {
        AppendBody::Flow { quantity } if *quantity > 0 => Ok((
            "flow",
            Some(i64::try_from(*quantity).map_err(|_| UsageStoreError::ArithmeticOverflow)?),
            None,
        )),
        AppendBody::Flow { .. } => Err(UsageStoreError::Invalid),
        AppendBody::Level { units } => Ok((
            "level",
            Some(i64::try_from(*units).map_err(|_| UsageStoreError::ArithmeticOverflow)?),
            None,
        )),
        AppendBody::Reversal { reverses } => Ok(("reversal", None, Some(reverses.as_str().to_owned()))),
    }
}

fn validate_source_body(source: UsageSource, occurred_at: DateTime<Utc>, body: &AppendBody) -> Result<(), UsageStoreError> {
    let valid = matches!(
        (source, body),
        (UsageSource::ConfirmedEnrollment, AppendBody::Flow { .. } | AppendBody::Reversal { .. })
            | (UsageSource::EligibleIdentificationCompleted, AppendBody::Flow { .. } | AppendBody::Reversal { .. })
            | (UsageSource::TemplateSample, AppendBody::Level { .. })
            | (UsageSource::PhotoStorageSample, AppendBody::Level { .. })
    );
    let midnight = occurred_at.time() == NaiveTime::from_hms_opt(0, 0, 0).expect("valid midnight");
    if valid && (matches!(body, AppendBody::Flow { .. } | AppendBody::Reversal { .. }) || midnight) {
        Ok(())
    } else {
        Err(UsageStoreError::Invalid)
    }
}

fn parse_meter(value: &str) -> Option<MeterId> {
    match value {
        "enrollments" => Some(MeterId::Enrollments),
        "retained_photo_storage_gb" => Some(MeterId::RetainedPhotoStorageGb),
        "active_templates" => Some(MeterId::ActiveTemplates),
        "identification_requests" => Some(MeterId::IdentificationRequests),
        _ => None,
    }
}

fn source_name(source: UsageSource) -> &'static str {
    match source {
        UsageSource::ConfirmedEnrollment => "confirmed_enrollment",
        UsageSource::EligibleIdentificationCompleted => "eligible_identification_completed",
        UsageSource::TemplateSample => "template_sample",
        UsageSource::PhotoStorageSample => "photo_storage_sample",
    }
}

fn meter_name(meter: MeterId) -> &'static str {
    match meter {
        MeterId::Enrollments => "enrollments",
        MeterId::RetainedPhotoStorageGb => "retained_photo_storage_gb",
        MeterId::ActiveTemplates => "active_templates",
        MeterId::IdentificationRequests => "identification_requests",
    }
}
