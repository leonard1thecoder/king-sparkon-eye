//! Supervised execution and transport for the durable #187 job/outbox tables.
//!
//! This module owns bounded scheduling, lease heartbeats, panic recovery by lease expiry,
//! graceful shutdown and at-least-once outbox delivery. Domain inference remains a
//! separately injected processor; the worker never invents biometric decisions.
use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::{Client, StatusCode, Url};
use serde_json::Value;
use tokio::{sync::watch, task::JoinSet, time::{interval, MissedTickBehavior}};
use tracing::{error, info, warn};

use crate::persistence::jobs::{
    ClaimedJob, ClaimedOutboxEvent, FailureClass, JobStore, JobStoreError,
};

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub worker_id: String,
    pub concurrency: usize,
    pub batch_size: i64,
    pub lease_ms: i64,
    pub poll_interval: Duration,
    pub outbox_max_attempts: i32,
}
impl WorkerConfig {
    pub fn validate(&self) -> Result<(), WorkerError> {
        let valid_worker = !self.worker_id.is_empty() && self.worker_id.len() <= 128
            && self.worker_id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':'));
        if !valid_worker || !(1..=64).contains(&self.concurrency)
            || !(1..=100).contains(&self.batch_size) || !(1..=900_000).contains(&self.lease_ms)
            || self.poll_interval.is_zero() || !(1..=20).contains(&self.outbox_max_attempts) {
            return Err(WorkerError::InvalidConfig);
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("worker configuration is invalid")]
    InvalidConfig,
    #[error("outbox transport configuration is invalid")]
    InvalidTransportConfig,
    #[error("worker persistence operation failed")]
    Persistence,
}

#[derive(Debug)]
pub enum JobExecution {
    /// A conclusive domain result, including negative/uncertain recognition outcomes.
    /// Domain outcomes are completed jobs, never infrastructure retries.
    Completed(Value),
    Retryable { category: &'static str, retry_after: Duration },
    Permanent { category: &'static str },
}

#[async_trait]
pub trait JobProcessor: Send + Sync + 'static {
    async fn process(&self, job: &ClaimedJob) -> JobExecution;
}

#[derive(Debug, Clone, Copy)]
pub struct TransportFailure {
    /// Stable uppercase category only; never include response bodies or credentials.
    pub category: &'static str,
    pub retryable: bool,
}

#[async_trait]
pub trait OutboxTransport: Send + Sync + 'static {
    async fn deliver(&self, event: &ClaimedOutboxEvent) -> Result<(), TransportFailure>;
}

#[derive(Clone)]
pub struct HttpOutboxTransport {
    client: Client,
    endpoint: Url,
    bearer_token: Arc<str>,
}
impl HttpOutboxTransport {
    /// Configures an operator-owned internal event receiver. This is not the signed
    /// customer webhook contract owned by #188.
    pub fn new(endpoint: &str, bearer_token: String, timeout: Duration) -> Result<Self, WorkerError> {
        let url = Url::parse(endpoint).map_err(|_| WorkerError::InvalidTransportConfig)?;
        let local_http = url.scheme() == "http"
            && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
        if (url.scheme() != "https" && !local_http) || url.host_str().is_none()
            || !url.username().is_empty() || url.password().is_some() || url.fragment().is_some()
            || bearer_token.trim().is_empty() || bearer_token.len() > 4096 || timeout.is_zero() {
            return Err(WorkerError::InvalidTransportConfig);
        }
        let client = Client::builder().timeout(timeout).build()
            .map_err(|_| WorkerError::InvalidTransportConfig)?;
        Ok(Self { client, endpoint: url, bearer_token: bearer_token.into() })
    }
}
#[async_trait]
impl OutboxTransport for HttpOutboxTransport {
    async fn deliver(&self, event: &ClaimedOutboxEvent) -> Result<(), TransportFailure> {
        let response = self.client.post(self.endpoint.clone())
            .bearer_auth(self.bearer_token.as_ref())
            .header("Idempotency-Key", &event.event_id)
            .header("X-Eye-Event-Type", &event.event_type)
            .json(&event.payload)
            .send().await
            .map_err(|_| TransportFailure { category: "TRANSPORT_UNAVAILABLE", retryable: true })?;
        let status = response.status();
        if status.is_success() { return Ok(()); }
        if status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
            || status == StatusCode::REQUEST_TIMEOUT {
            return Err(TransportFailure { category: "TRANSPORT_RETRYABLE_STATUS", retryable: true });
        }
        Err(TransportFailure { category: "TRANSPORT_REJECTED", retryable: false })
    }
}

#[derive(Clone)]
pub struct WorkerRuntime {
    store: JobStore,
    processor: Arc<dyn JobProcessor>,
    transport: Arc<dyn OutboxTransport>,
    config: WorkerConfig,
}
impl WorkerRuntime {
    pub fn new(store: JobStore, processor: Arc<dyn JobProcessor>,
        transport: Arc<dyn OutboxTransport>, config: WorkerConfig) -> Result<Self, WorkerError> {
        config.validate()?;
        Ok(Self { store, processor, transport, config })
    }

    /// Runs independent job and outbox supervisors. A watch signal stops new claims,
    /// then drains in-flight tasks; leases fence any task that cannot finish in time.
    pub async fn run(&self, shutdown: watch::Receiver<bool>) -> Result<(), WorkerError> {
        let jobs = self.run_job_supervisor(shutdown.clone());
        let outbox = self.run_outbox_supervisor(shutdown);
        tokio::try_join!(jobs, outbox)?;
        Ok(())
    }

    async fn run_job_supervisor(&self, mut shutdown: watch::Receiver<bool>) -> Result<(), WorkerError> {
        let mut tasks = JoinSet::new();
        let mut poll = interval(self.config.poll_interval);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut stopping = *shutdown.borrow();
        while !stopping || !tasks.is_empty() {
            if stopping {
                if let Some(result) = tasks.join_next().await { log_join(result, "job"); }
                continue;
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { stopping = true; }
                }
                joined = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(result) = joined { log_join(result, "job"); }
                }
                _ = poll.tick() => {
                    let available = self.config.concurrency.saturating_sub(tasks.len());
                    if available == 0 { continue; }
                    let claims = self.store.claim(&self.config.worker_id, now_ms(),
                        self.config.lease_ms, (available as i64).min(self.config.batch_size))
                        .await.map_err(map_store_error)?;
                    for claim in claims {
                        let store = self.store.clone();
                        let processor = self.processor.clone();
                        let worker_id = self.config.worker_id.clone();
                        let lease_ms = self.config.lease_ms;
                        tasks.spawn(async move {
                            execute_claim(store, processor, claim, worker_id, lease_ms).await;
                        });
                    }
                    if let Err(err) = self.store.recover_exhausted_leases(now_ms(), self.config.batch_size).await {
                        warn!(error = %err, "job lease recovery pass failed");
                    }
                }
            }
        }
        Ok(())
    }

    async fn run_outbox_supervisor(&self, mut shutdown: watch::Receiver<bool>) -> Result<(), WorkerError> {
        let mut tasks = JoinSet::new();
        let mut poll = interval(self.config.poll_interval);
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut stopping = *shutdown.borrow();
        while !stopping || !tasks.is_empty() {
            if stopping {
                if let Some(result) = tasks.join_next().await { log_join(result, "outbox"); }
                continue;
            }
            tokio::select! {
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { stopping = true; }
                }
                joined = tasks.join_next(), if !tasks.is_empty() => {
                    if let Some(result) = joined { log_join(result, "outbox"); }
                }
                _ = poll.tick() => {
                    let available = self.config.concurrency.saturating_sub(tasks.len());
                    if available == 0 { continue; }
                    let claims = self.store.claim_outbox(&self.config.worker_id, now_ms(),
                        self.config.lease_ms, (available as i64).min(self.config.batch_size))
                        .await.map_err(map_store_error)?;
                    for event in claims {
                        let store = self.store.clone();
                        let transport = self.transport.clone();
                        let max_attempts = self.config.outbox_max_attempts;
                        tasks.spawn(async move { deliver_claim(store, transport, event, max_attempts).await; });
                    }
                }
            }
        }
        Ok(())
    }
}

async fn execute_claim(store: JobStore, processor: Arc<dyn JobProcessor>, claim: ClaimedJob,
    worker_id: String, lease_ms: i64) {
    let job_id = claim.job.job_id.clone();
    let token = claim.lease_token;
    let mut execution = Box::pin(processor.process(&claim));
    let heartbeat_every = Duration::from_millis((lease_ms / 3).max(250) as u64);
    let mut heartbeat = interval(heartbeat_every);
    heartbeat.set_missed_tick_behavior(MissedTickBehavior::Delay);
    // Do not let the interval's immediate first tick renew before processing begins.
    heartbeat.tick().await;
    let outcome = loop {
        tokio::select! {
            result = &mut execution => break result,
            _ = heartbeat.tick() => {
                if store.heartbeat(&job_id, token, &worker_id, now_ms(), lease_ms).await.is_err() {
                    warn!(job_id = %job_id, "job lease lost; cancelling local execution");
                    return;
                }
            }
        }
    };
    let now = now_ms();
    let result = match outcome {
        JobExecution::Completed(value) => store.finalize(&job_id, token, &worker_id, "COMPLETED", Some(value), now).await.map(|_| ()),
        JobExecution::Retryable { category, retry_after } => store.fail(&job_id, token, &worker_id,
            category, FailureClass::Retryable, now, retry_after.as_millis().min(i64::MAX as u128) as i64).await.map(|_| ()),
        JobExecution::Permanent { category } => store.fail(&job_id, token, &worker_id,
            category, FailureClass::Permanent, now, 0).await.map(|_| ()),
    };
    if let Err(err) = result { warn!(job_id = %job_id, error = %err, "job finalization failed; lease recovery will reconcile"); }
}

async fn deliver_claim(store: JobStore, transport: Arc<dyn OutboxTransport>,
    event: ClaimedOutboxEvent, max_attempts: i32) {
    let result = transport.deliver(&event).await;
    let now = now_ms();
    match result {
        Ok(()) => match store.acknowledge_outbox(&event.event_id, event.lease_token, now).await {
            Ok(true) => info!(event_id = %event.event_id, "outbox event delivered"),
            Ok(false) => warn!(event_id = %event.event_id, "outbox acknowledgement lost its lease"),
            Err(err) => warn!(event_id = %event.event_id, error = %err, "outbox acknowledgement failed"),
        },
        Err(failure) => {
            let delay = retry_delay_ms(event.attempts);
            let retry = store.retry_outbox(&event.event_id, event.lease_token, failure.category,
                now, delay, if failure.retryable { max_attempts } else { 1 }).await;
            if let Err(err) = retry { warn!(event_id = %event.event_id, error = %err, "outbox retry scheduling failed"); }
        }
    }
}

pub fn retry_delay_ms(attempt: i32) -> i64 {
    let exponent = attempt.saturating_sub(1).clamp(0, 10) as u32;
    1_000_i64.saturating_mul(1_i64 << exponent).min(300_000)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis().min(i64::MAX as u128) as i64).unwrap_or(0)
}
fn map_store_error(_: JobStoreError) -> WorkerError { WorkerError::Persistence }
fn log_join(result: Result<(), tokio::task::JoinError>, subsystem: &'static str) {
    if let Err(err) = result { error!(subsystem, error = %err, "worker task panicked or was cancelled; durable lease recovery will reclaim work"); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_is_exponential_and_bounded() {
        assert_eq!(retry_delay_ms(1), 1_000);
        assert_eq!(retry_delay_ms(2), 2_000);
        assert_eq!(retry_delay_ms(3), 4_000);
        assert_eq!(retry_delay_ms(99), 300_000);
    }

    #[test]
    fn worker_config_rejects_unbounded_or_invalid_values() {
        let valid = WorkerConfig { worker_id: "eye-worker-1".into(), concurrency: 4,
            batch_size: 8, lease_ms: 30_000, poll_interval: Duration::from_millis(500),
            outbox_max_attempts: 8 };
        assert!(valid.validate().is_ok());
        let mut invalid = valid.clone();
        invalid.concurrency = 0;
        assert!(matches!(invalid.validate(), Err(WorkerError::InvalidConfig)));
        let mut invalid = valid;
        invalid.lease_ms = 900_001;
        assert!(matches!(invalid.validate(), Err(WorkerError::InvalidConfig)));
    }

    #[test]
    fn internal_transport_requires_tls_and_a_bounded_secret() {
        assert!(HttpOutboxTransport::new("https://events.example.test/ingest", "secret".into(), Duration::from_secs(5)).is_ok());
        assert!(HttpOutboxTransport::new("http://events.example.test/ingest", "secret".into(), Duration::from_secs(5)).is_err());
        assert!(HttpOutboxTransport::new("https://user:pass@events.example.test/ingest", "secret".into(), Duration::from_secs(5)).is_err());
        assert!(HttpOutboxTransport::new("https://events.example.test/ingest", "".into(), Duration::from_secs(5)).is_err());
    }
}
