//! Durable monthly Stripe Meter outbox worker (#143).
//!
//! The worker claims persisted reporting intents, folds them into tenant/meter/month groups,
//! computes a closed-month aggregate from PostgreSQL, resolves the Stripe Customer only through
//! a trusted server-side billing-account resolver, and acknowledges only after Stripe accepts
//! the aggregate. It never reads source payloads or sends internal tenant identifiers to Stripe.
//!
//! The worker is not runtime-wired. Deployments must explicitly construct it with an enabled
//! test-mode reporter and an authoritative customer resolver after the #138 contract is approved.

use std::collections::BTreeMap;

use async_trait::async_trait;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use thiserror::Error;

use crate::{
    billing::{
        AllowancePeriod, MeterId,
        meter_reporting::{
            MonthlyMeterAggregate, StripeMeterError, StripeMeterReporter,
        },
    },
    persistence::usage_ledger::{
        ClaimedUsageReport, MonthlyUsageAggregate, UsageLedgerStore, UsageStoreError,
    },
};

/// Trusted lookup for a tenant's Stripe Customer mapping.
///
/// Implementations must query server-owned billing-account state. They must not accept a
/// browser-supplied Customer ID or derive one from usage events.
#[async_trait]
pub trait StripeCustomerResolver: Send + Sync {
    async fn resolve_customer(
        &self,
        tenant_reference: &str,
    ) -> Result<Option<String>, CustomerResolveError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum CustomerResolveError {
    #[error("trusted billing customer mapping is temporarily unavailable")]
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportingWorkerConfig {
    pub lease: Duration,
    pub batch_size: i64,
    pub max_attempts: i32,
    pub retry_base: Duration,
    pub retry_max: Duration,
}

impl Default for ReportingWorkerConfig {
    fn default() -> Self {
        Self {
            lease: Duration::minutes(2),
            batch_size: 100,
            max_attempts: 12,
            retry_base: Duration::seconds(30),
            retry_max: Duration::hours(6),
        }
    }
}

impl ReportingWorkerConfig {
    fn validate(self) -> Result<Self, WorkerError> {
        if self.lease <= Duration::zero()
            || !(1..=100).contains(&self.batch_size)
            || self.max_attempts <= 0
            || self.retry_base <= Duration::zero()
            || self.retry_max < self.retry_base
        {
            return Err(WorkerError::InvalidConfiguration);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WorkerBatchResult {
    pub claimed_rows: usize,
    pub report_groups: usize,
    pub reported_rows: usize,
    pub retried_rows: usize,
    pub dead_lettered_rows: usize,
    pub lease_lost_rows: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WorkerError {
    #[error("usage reporting worker configuration is invalid")]
    InvalidConfiguration,
    #[error("usage reporting persistence operation failed")]
    Persistence,
}

pub struct UsageReportingWorker<R> {
    store: UsageLedgerStore,
    reporter: StripeMeterReporter,
    customer_resolver: R,
    config: ReportingWorkerConfig,
}

impl<R: StripeCustomerResolver> UsageReportingWorker<R> {
    pub fn new(
        store: UsageLedgerStore,
        reporter: StripeMeterReporter,
        customer_resolver: R,
        config: ReportingWorkerConfig,
    ) -> Result<Self, WorkerError> {
        Ok(Self {
            store,
            reporter,
            customer_resolver,
            config: config.validate()?,
        })
    }

    /// Process at most one claimed batch. Call periodically from a supervised runtime task.
    ///
    /// Rows are grouped by tenant/meter/month before sending. If a process crashes after Stripe
    /// accepts an event but before PostgreSQL acknowledgement, the same stable Stripe identifier
    /// is sent on replay. A stale lease cannot acknowledge a newer attempt.
    pub async fn run_once(
        &self,
        now: DateTime<Utc>,
    ) -> Result<WorkerBatchResult, WorkerError> {
        let claimed = self
            .store
            .claim_due(now, self.config.lease, self.config.batch_size)
            .await
            .map_err(|_| WorkerError::Persistence)?;

        let mut result = WorkerBatchResult {
            claimed_rows: claimed.len(),
            ..WorkerBatchResult::default()
        };
        let mut groups: BTreeMap<(String, MeterId, NaiveDate), Vec<ClaimedUsageReport>> =
            BTreeMap::new();
        for row in claimed {
            groups
                .entry((row.tenant_id.clone(), row.meter, row.period_start))
                .or_default()
                .push(row);
        }
        result.report_groups = groups.len();

        for ((tenant, meter, period_start), rows) in groups {
            let period = period_for(period_start).ok_or(WorkerError::Persistence)?;
            let aggregate = match self
                .store
                .monthly_aggregate(&tenant, &period, now)
                .await
            {
                Ok(aggregate) => aggregate,
                Err(error) => {
                    let category = aggregate_error_category(&error);
                    self.retry_group(&rows, now, category, &mut result).await?;
                    continue;
                }
            };

            let quantity = aggregate.quantity(meter);
            // Zero usage is a completed report with no Stripe event. Sending a zero meter event
            // is unnecessary and some meter configurations reject it.
            if quantity == 0 {
                self.ack_group(&rows, now, &mut result).await?;
                continue;
            }

            let customer_id = match self.customer_resolver.resolve_customer(&tenant).await {
                Ok(Some(customer_id)) => customer_id,
                Ok(None) => {
                    self.retry_group(
                        &rows,
                        now,
                        "customer_mapping_missing",
                        &mut result,
                    )
                    .await?;
                    continue;
                }
                Err(CustomerResolveError::Unavailable) => {
                    self.retry_group(
                        &rows,
                        now,
                        "customer_mapping_unavailable",
                        &mut result,
                    )
                    .await?;
                    continue;
                }
            };

            let stripe_aggregate = MonthlyMeterAggregate {
                stripe_customer_id: customer_id,
                meter,
                period_start,
                quantity,
            };
            match self
                .reporter
                .report_monthly(&tenant, &stripe_aggregate)
                .await
            {
                Ok(()) => self.ack_group(&rows, now, &mut result).await?,
                Err(error) => {
                    let (category, retryable) = stripe_error_category(error);
                    if retryable {
                        self.retry_group(&rows, now, category, &mut result).await?;
                    } else {
                        self.dead_letter_group(&rows, now, category, &mut result).await?;
                    }
                }
            }

            // Keep the type explicit: aggregate tenant identity is internal and never serialized.
            let _internal_tenant = &aggregate.tenant_id;
        }

        Ok(result)
    }

    async fn ack_group(
        &self,
        rows: &[ClaimedUsageReport],
        now: DateTime<Utc>,
        result: &mut WorkerBatchResult,
    ) -> Result<(), WorkerError> {
        for row in rows {
            if self
                .store
                .mark_reported(row.id, row.attempts, now)
                .await
                .map_err(|_| WorkerError::Persistence)?
            {
                result.reported_rows += 1;
            } else {
                result.lease_lost_rows += 1;
            }
        }
        Ok(())
    }

    async fn retry_group(
        &self,
        rows: &[ClaimedUsageReport],
        now: DateTime<Utc>,
        category: &'static str,
        result: &mut WorkerBatchResult,
    ) -> Result<(), WorkerError> {
        for row in rows {
            let delay = retry_delay(self.config, row.attempts);
            let next = now
                .checked_add_signed(delay)
                .ok_or(WorkerError::InvalidConfiguration)?;
            let changed = self
                .store
                .schedule_retry(
                    row.id,
                    row.attempts,
                    now,
                    next,
                    category,
                    self.config.max_attempts,
                )
                .await
                .map_err(|_| WorkerError::Persistence)?;
            if changed {
                if row.attempts >= self.config.max_attempts {
                    result.dead_lettered_rows += 1;
                } else {
                    result.retried_rows += 1;
                }
            } else {
                result.lease_lost_rows += 1;
            }
        }
        Ok(())
    }

    async fn dead_letter_group(
        &self,
        rows: &[ClaimedUsageReport],
        now: DateTime<Utc>,
        category: &'static str,
        result: &mut WorkerBatchResult,
    ) -> Result<(), WorkerError> {
        for row in rows {
            // A permanent provider rejection must not loop indefinitely. The row is still
            // protected by its attempt token, so an expired/reclaimed lease cannot be overwritten.
            let next = now
                .checked_add_signed(Duration::zero())
                .ok_or(WorkerError::InvalidConfiguration)?;
            let changed = self
                .store
                .schedule_retry(
                    row.id,
                    row.attempts,
                    now,
                    next,
                    category,
                    row.attempts.max(1),
                )
                .await
                .map_err(|_| WorkerError::Persistence)?;
            if changed {
                result.retried_rows = result.retried_rows.saturating_sub(1);
                result.dead_lettered_rows += 1;
            }
        }
        Ok(())
    }
}

fn period_for(start: NaiveDate) -> Option<AllowancePeriod> {
    if start.day() != 1 {
        return None;
    }
    Some(AllowancePeriod::containing(
        start.and_hms_opt(0, 0, 0)?.and_utc(),
    ))
}

fn aggregate_error_category(error: &UsageStoreError) -> &'static str {
    match error {
        UsageStoreError::PeriodNotClosed => "period_not_closed",
        UsageStoreError::MissingLevelSample => "level_sample_missing",
        UsageStoreError::Invalid | UsageStoreError::InvalidWindow => "aggregate_invalid",
        UsageStoreError::ArithmeticOverflow => "aggregate_overflow",
        _ => "ledger_unavailable",
    }
}

fn stripe_error_category(error: StripeMeterError) -> (&'static str, bool) {
    match error {
        StripeMeterError::Transport => ("stripe_transport", true),
        StripeMeterError::Rejected => ("stripe_rejected", false),
        StripeMeterError::UnexpectedResponse => ("stripe_unexpected_response", true),
        StripeMeterError::Disabled => ("stripe_reporting_disabled", false),
        StripeMeterError::InvalidConfiguration => ("stripe_configuration_invalid", false),
    }
}

fn retry_delay(config: ReportingWorkerConfig, attempts: i32) -> Duration {
    let exponent = u32::try_from(attempts.saturating_sub(1)).unwrap_or(u32::MAX).min(20);
    let factor = 1_i32.checked_shl(exponent).unwrap_or(i32::MAX);
    let seconds = config
        .retry_base
        .num_seconds()
        .saturating_mul(i64::from(factor))
        .min(config.retry_max.num_seconds());
    Duration::seconds(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_grows_exponentially_and_is_capped() {
        let config = ReportingWorkerConfig::default();
        assert_eq!(retry_delay(config, 1), Duration::seconds(30));
        assert_eq!(retry_delay(config, 2), Duration::seconds(60));
        assert_eq!(retry_delay(config, 3), Duration::seconds(120));
        assert_eq!(retry_delay(config, 20), Duration::hours(6));
    }

    #[test]
    fn only_first_day_of_month_can_be_a_reporting_period() {
        assert!(period_for(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap()).is_some());
        assert!(period_for(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap()).is_none());
    }

    #[test]
    fn worker_rejects_unsafe_retry_configuration() {
        let invalid = ReportingWorkerConfig {
            retry_base: Duration::hours(2),
            retry_max: Duration::hours(1),
            ..ReportingWorkerConfig::default()
        };
        assert_eq!(invalid.validate(), Err(WorkerError::InvalidConfiguration));
    }
}
