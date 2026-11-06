//! Supervised scheduling and process-local operational metrics for Stripe usage reporting.
//!
//! This module deliberately provides a runtime component rather than constructing the worker.
//! The application must inject the approved Stripe customer resolver and test-mode reporter once
//! #138 is implemented. Until then, no billing task is started by the HTTP service.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration as StdDuration, Instant},
};

use async_trait::async_trait;
use chrono::Utc;
use tokio::{sync::watch, time::{self, MissedTickBehavior}};
use tracing::{error, info, warn};

use super::reporting_worker::{
    ReportingWorkerConfig, StripeCustomerResolver, UsageReportingWorker, WorkerBatchResult,
    WorkerError,
};

/// Scheduling policy. A single run is executed at a time; missed ticks are skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportingRuntimeConfig {
    pub poll_interval: StdDuration,
}

impl Default for ReportingRuntimeConfig {
    fn default() -> Self {
        Self {
            poll_interval: StdDuration::from_secs(30),
        }
    }
}

impl ReportingRuntimeConfig {
    pub fn validate(self) -> Result<Self, ReportingRuntimeError> {
        if self.poll_interval.is_zero() || self.poll_interval > StdDuration::from_secs(3600) {
            return Err(ReportingRuntimeError::InvalidConfiguration);
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ReportingRuntimeError {
    #[error("Stripe reporting runtime configuration is invalid")]
    InvalidConfiguration,
}

/// Runtime-neutral contract so the scheduler can be tested without PostgreSQL or Stripe.
#[async_trait]
pub trait ReportingBatchProcessor: Send + Sync + 'static {
    async fn run_reporting_batch(&self) -> Result<WorkerBatchResult, WorkerError>;
}

#[async_trait]
impl<R> ReportingBatchProcessor for UsageReportingWorker<R>
where
    R: StripeCustomerResolver + 'static,
{
    async fn run_reporting_batch(&self) -> Result<WorkerBatchResult, WorkerError> {
        self.run_once(Utc::now()).await
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReportingMetricsSnapshot {
    pub batches_started: u64,
    pub batches_succeeded: u64,
    pub batches_failed: u64,
    pub claimed_rows: u64,
    pub reported_rows: u64,
    pub retried_rows: u64,
    pub dead_lettered_rows: u64,
    pub lease_lost_rows: u64,
    pub consecutive_failures: u64,
    pub last_success_unix_seconds: Option<i64>,
    pub last_batch_duration_ms: u64,
}

#[derive(Default)]
pub struct ReportingRuntimeMetrics {
    batches_started: AtomicU64,
    batches_succeeded: AtomicU64,
    batches_failed: AtomicU64,
    claimed_rows: AtomicU64,
    reported_rows: AtomicU64,
    retried_rows: AtomicU64,
    dead_lettered_rows: AtomicU64,
    lease_lost_rows: AtomicU64,
    consecutive_failures: AtomicU64,
    last_success_unix_seconds: AtomicU64,
    last_batch_duration_ms: AtomicU64,
}

impl ReportingRuntimeMetrics {
    pub fn snapshot(&self) -> ReportingMetricsSnapshot {
        let last_success = self.last_success_unix_seconds.load(Ordering::Relaxed);
        ReportingMetricsSnapshot {
            batches_started: self.batches_started.load(Ordering::Relaxed),
            batches_succeeded: self.batches_succeeded.load(Ordering::Relaxed),
            batches_failed: self.batches_failed.load(Ordering::Relaxed),
            claimed_rows: self.claimed_rows.load(Ordering::Relaxed),
            reported_rows: self.reported_rows.load(Ordering::Relaxed),
            retried_rows: self.retried_rows.load(Ordering::Relaxed),
            dead_lettered_rows: self.dead_lettered_rows.load(Ordering::Relaxed),
            lease_lost_rows: self.lease_lost_rows.load(Ordering::Relaxed),
            consecutive_failures: self.consecutive_failures.load(Ordering::Relaxed),
            last_success_unix_seconds: (last_success != 0).then_some(last_success as i64),
            last_batch_duration_ms: self.last_batch_duration_ms.load(Ordering::Relaxed),
        }
    }

    fn record_success(&self, batch: WorkerBatchResult, elapsed: StdDuration) {
        self.batches_succeeded.fetch_add(1, Ordering::Relaxed);
        self.claimed_rows.fetch_add(batch.claimed_rows as u64, Ordering::Relaxed);
        self.reported_rows.fetch_add(batch.reported_rows as u64, Ordering::Relaxed);
        self.retried_rows.fetch_add(batch.retried_rows as u64, Ordering::Relaxed);
        self.dead_lettered_rows.fetch_add(batch.dead_lettered_rows as u64, Ordering::Relaxed);
        self.lease_lost_rows.fetch_add(batch.lease_lost_rows as u64, Ordering::Relaxed);
        self.consecutive_failures.store(0, Ordering::Relaxed);
        self.last_success_unix_seconds
            .store(Utc::now().timestamp().max(0) as u64, Ordering::Relaxed);
        self.last_batch_duration_ms
            .store(elapsed.as_millis().min(u128::from(u64::MAX)) as u64, Ordering::Relaxed);
    }

    fn record_failure(&self, elapsed: StdDuration) {
        self.batches_failed.fetch_add(1, Ordering::Relaxed);
        self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        self.last_batch_duration_ms
            .store(elapsed.as_millis().min(u128::from(u64::MAX)) as u64, Ordering::Relaxed);
    }
}

/// Runs one reporting batch per interval until the shutdown watch is set or closed.
///
/// A batch is never overlapped with another batch in this process. Database leases still
/// coordinate multiple service replicas. A failed batch is logged and the loop remains alive;
/// durable rows stay leased/retryable according to the worker's persistence state machine.
pub async fn run_reporting_scheduler<W>(
    processor: Arc<W>,
    config: ReportingRuntimeConfig,
    metrics: Arc<ReportingRuntimeMetrics>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), ReportingRuntimeError>
where
    W: ReportingBatchProcessor,
{
    let config = config.validate()?;
    let mut ticker = time::interval(config.poll_interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);

    info!(
        poll_interval_seconds = config.poll_interval.as_secs(),
        "Stripe usage reporting scheduler started"
    );

    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    info!("Stripe usage reporting scheduler stopping");
                    return Ok(());
                }
            }
            _ = ticker.tick() => {
                metrics.batches_started.fetch_add(1, Ordering::Relaxed);
                let started = Instant::now();
                match processor.run_reporting_batch().await {
                    Ok(batch) => {
                        let elapsed = started.elapsed();
                        metrics.record_success(batch, elapsed);
                        info!(
                            claimed_rows = batch.claimed_rows,
                            report_groups = batch.report_groups,
                            reported_rows = batch.reported_rows,
                            retried_rows = batch.retried_rows,
                            dead_lettered_rows = batch.dead_lettered_rows,
                            lease_lost_rows = batch.lease_lost_rows,
                            duration_ms = elapsed.as_millis() as u64,
                            "Stripe usage reporting batch completed"
                        );
                        if batch.dead_lettered_rows > 0 {
                            warn!(
                                dead_lettered_rows = batch.dead_lettered_rows,
                                "Stripe usage reporting produced dead letters"
                            );
                        }
                    }
                    Err(err) => {
                        let elapsed = started.elapsed();
                        metrics.record_failure(elapsed);
                        error!(
                            error = %err,
                            duration_ms = elapsed.as_millis() as u64,
                            consecutive_failures = metrics.consecutive_failures.load(Ordering::Relaxed),
                            "Stripe usage reporting batch failed"
                        );
                    }
                }
            }
        }
    }
}

/// Builds a worker using the worker's validated retry/lease configuration. Kept as a
/// convenience for callers that assemble billing dependencies outside the HTTP router.
pub fn default_worker_config() -> ReportingWorkerConfig {
    ReportingWorkerConfig::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_rejects_zero_and_excessive_intervals() {
        assert_eq!(
            ReportingRuntimeConfig { poll_interval: StdDuration::ZERO }.validate(),
            Err(ReportingRuntimeError::InvalidConfiguration)
        );
        assert_eq!(
            ReportingRuntimeConfig { poll_interval: StdDuration::from_secs(3601) }.validate(),
            Err(ReportingRuntimeError::InvalidConfiguration)
        );
        assert!(ReportingRuntimeConfig::default().validate().is_ok());
    }

    #[test]
    fn metrics_snapshot_starts_empty() {
        let metrics = ReportingRuntimeMetrics::default();
        assert_eq!(metrics.snapshot(), ReportingMetricsSnapshot::default());
    }

    #[test]
    fn success_resets_consecutive_failures_and_accumulates_delivery_counts() {
        let metrics = ReportingRuntimeMetrics::default();
        metrics.record_failure(StdDuration::from_millis(4));
        metrics.record_success(
            WorkerBatchResult {
                claimed_rows: 5,
                report_groups: 2,
                reported_rows: 3,
                retried_rows: 1,
                dead_lettered_rows: 1,
                lease_lost_rows: 0,
            },
            StdDuration::from_millis(12),
        );
        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.batches_started, 0);
        assert_eq!(snapshot.batches_failed, 1);
        assert_eq!(snapshot.batches_succeeded, 1);
        assert_eq!(snapshot.claimed_rows, 5);
        assert_eq!(snapshot.reported_rows, 3);
        assert_eq!(snapshot.retried_rows, 1);
        assert_eq!(snapshot.dead_lettered_rows, 1);
        assert_eq!(snapshot.consecutive_failures, 0);
        assert!(snapshot.last_success_unix_seconds.is_some());
        assert_eq!(snapshot.last_batch_duration_ms, 12);
    }
}
