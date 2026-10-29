//! Versioned Kafka enrollment events (issue #28).
//!
//! Publishes notification events after an enrollment photo passes deterministic assessment
//! and is securely stored by [`crate::storage::EnrollmentService`].
//!
//! Non-negotiable rules enforced here:
//! - Under no circumstance are raw photo bytes, embeddings, or biometric templates placed in Kafka event payloads.
//! - Event schema is typed, versioned (`v1`), and self-contained.
//! - Idempotency key and enrollment ID enable deduplication on downstream consumers (Spring Boot).
//! - The current synchronous publisher uses a bounded attempt count only. Backoff/jitter, durable retries,
//!   dead-lettering and a real broker adapter are specified in docs/KAFKA_ENROLLMENT_CONTRACT.md but not implemented.
//! - Clear separation of storage success and event publication failure: publishing failures
//!   never mark persistence as Confirmed and never corrupt or silently discard stored metadata.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::contract::{PersistenceStatus, validate_identifier};
use crate::error::ValidationError;
use crate::storage::{EnrollmentRecord, EnrollmentStatus};

/// Current topic for photo enrollment lifecycle events.
pub const ENROLLMENT_EVENTS_TOPIC: &str = "eye.enrollments.v1";

/// Current schema version string.
pub const ENROLLMENT_EVENT_VERSION: &str = "1.0.0";

/// Canonical event type identifier.
pub const ENROLLMENT_EVENT_TYPE: &str = "photo.enrolled.v1";

/// Dimensions metadata of the enrolled image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageDimensions {
    pub width: u32,
    pub height: u32,
}

/// Versioned enrollment event payload sent to Kafka.
///
/// Contains metadata only. NEVER contains raw bytes, embeddings, or credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoEnrolledEventV1 {
    pub event_id: String,
    pub event_type: String,
    pub event_version: String,
    pub enrollment_id: String,
    pub object_key: String,
    pub content_sha256: String,
    pub byte_len: usize,
    pub dimensions: ImageDimensions,
    pub consent_reference: String,
    pub policy_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    pub persistence: PersistenceStatus,
    pub occurred_at_ms: u64,
}

impl PhotoEnrolledEventV1 {
    /// Builds an event payload from an existing [`EnrollmentRecord`].
    pub fn from_record(
        record: &EnrollmentRecord,
        event_id: String,
        occurred_at_ms: u64,
    ) -> Result<Self, KafkaPublishError> {
        // EnrollmentService creates records only after assessment passes, but its records are
        // process-local and consent is only syntax-checked. This layer can at least prevent
        // deleted/failed records from becoming events; it cannot stand in for authoritative
        // consent, caller authorization, or a durable outbox.
        if record.status != EnrollmentStatus::Stored
            || matches!(
                record.persistence,
                PersistenceStatus::Failed | PersistenceStatus::NotRequested
            )
        {
            return Err(KafkaPublishError::IneligibleEnrollment);
        }
        Ok(Self {
            event_id,
            event_type: ENROLLMENT_EVENT_TYPE.to_string(),
            event_version: ENROLLMENT_EVENT_VERSION.to_string(),
            enrollment_id: record.enrollment_id.clone(),
            object_key: record.object_key.clone(),
            content_sha256: record.content_sha256.clone(),
            byte_len: record.byte_len,
            dimensions: ImageDimensions {
                width: record.width,
                height: record.height,
            },
            consent_reference: record.consent_reference.clone(),
            policy_version: record.policy_version.clone(),
            model_version: record.model_version.clone(),
            idempotency_key: record.idempotency_key.clone(),
            persistence: record.persistence,
            occurred_at_ms,
        })
    }

    /// Validates event structural integrity before publishing.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_identifier("eventId", &self.event_id)?;
        validate_identifier("enrollmentId", &self.enrollment_id)?;
        validate_identifier("consentReference", &self.consent_reference)?;
        if let Some(ref idem) = self.idempotency_key {
            validate_identifier("idempotencyKey", idem)?;
        }
        if self.event_type.is_empty() {
            return Err(ValidationError {
                field: "eventType",
                rule: "must not be empty",
            });
        }
        if self.content_sha256.len() != 64
            || !self
                .content_sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(ValidationError {
                field: "contentSha256",
                rule: "must be a valid 64-character hex SHA-256 digest",
            });
        }
        Ok(())
    }
}

/// A published event record with partition key and serialized JSON message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedMessage {
    pub topic: String,
    pub key: String,
    pub payload_json: String,
}

/// Errors from Kafka publishing operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KafkaPublishError {
    #[error("Kafka broker is unavailable")]
    Unavailable,
    #[error("event serialization failed")]
    SerializationError(String),
    #[error("event payload validation failed: {0}")]
    ValidationError(#[from] ValidationError),
    #[error("publish request timed out")]
    Timeout,
    #[error("maximum retry attempts exceeded")]
    RetriesExceeded,
    #[error("enrollment is not eligible for an enrollment event")]
    IneligibleEnrollment,
    #[error("idempotency key was already used for a different enrollment")]
    IdempotencyConflict,
}

/// Trait defining a Kafka producer interface. The future resolves only after broker acknowledgement.
pub trait KafkaProducer: Send + Sync {
    fn publish<'a>(
        &'a self,
        topic: &'a str,
        key: &'a str,
        payload_json: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), KafkaPublishError>> + Send + 'a>>;
}

/// Thread-safe in-memory Kafka producer for tests and verification without network brokers.
#[derive(Debug, Default)]
pub struct MemoryKafkaProducer {
    messages: Mutex<Vec<PublishedMessage>>,
    fail_next_publishes: Mutex<u32>,
}

impl MemoryKafkaProducer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fail_next_publishes(&self, count: u32) {
        if let Ok(mut guard) = self.fail_next_publishes.lock() {
            *guard = count;
        }
    }

    pub fn published_messages(&self) -> Vec<PublishedMessage> {
        self.messages.lock().map(|m| m.clone()).unwrap_or_default()
    }

    pub fn clear(&self) {
        if let Ok(mut guard) = self.messages.lock() {
            guard.clear();
        }
    }
}

impl KafkaProducer for MemoryKafkaProducer {
    fn publish<'a>(
        &'a self,
        topic: &'a str,
        key: &'a str,
        payload_json: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), KafkaPublishError>> + Send + 'a>> {
        Box::pin(async move {
            {
                let mut fails = self
                    .fail_next_publishes
                    .lock()
                    .map_err(|_| KafkaPublishError::Unavailable)?;
                if *fails > 0 {
                    *fails -= 1;
                    return Err(KafkaPublishError::Unavailable);
                }
            }

            let mut list = self
                .messages
                .lock()
                .map_err(|_| KafkaPublishError::Unavailable)?;
            list.push(PublishedMessage {
                topic: topic.to_string(),
                key: key.to_string(),
                payload_json: payload_json.to_string(),
            });
            Ok(())
        })
    }
}

/// Retry policy for event publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_ms: 25,
            max_delay_ms: 500,
        }
    }
}

/// Publisher service orchestrating Kafka publishing and retry logic for enrollment events.
pub struct EnrollmentEventPublisher<P: KafkaProducer> {
    producer: P,
    retry_policy: RetryPolicy,
    topic: String,
    published_idempotency: Mutex<HashMap<String, (String, String)>>,
}

impl<P: KafkaProducer> EnrollmentEventPublisher<P> {
    pub fn new(producer: P) -> Self {
        Self {
            producer,
            retry_policy: RetryPolicy::default(),
            topic: ENROLLMENT_EVENTS_TOPIC.to_string(),
            published_idempotency: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_retry_policy(mut self, retry_policy: RetryPolicy) -> Self {
        self.retry_policy = retry_policy;
        self
    }

    pub fn with_topic(mut self, topic: &str) -> Self {
        self.topic = topic.to_string();
        self
    }

    pub fn producer(&self) -> &P {
        &self.producer
    }

    /// Publishes a [`PhotoEnrolledEventV1`] with retry and idempotency tracking.
    pub async fn publish_enrollment_event(
        &self,
        event: &PhotoEnrolledEventV1,
    ) -> Result<String, KafkaPublishError> {
        event.validate()?;

        // Idempotency check: if an idempotency_key was supplied and already published with
        // identical enrollmentId, return success immediately without emitting duplicate Kafka message.
        if let Some(ref idem) = event.idempotency_key {
            let map = self
                .published_idempotency
                .lock()
                .map_err(|_| KafkaPublishError::Unavailable)?;
            if let Some((existing_event_id, existing_enrollment_id)) = map.get(idem) {
                if existing_enrollment_id == &event.enrollment_id {
                    return Ok(existing_event_id.clone());
                }
                return Err(KafkaPublishError::IdempotencyConflict);
            }
        }

        let payload_json = serde_json::to_string(event)
            .map_err(|e| KafkaPublishError::SerializationError(e.to_string()))?;

        // Stable per-enrollment partitioning preserves ordering for this enrollment.
        // Idempotency is a separate concern and must not change the partition key.
        let partition_key = &event.enrollment_id;

        let mut attempts = 0;
        let mut last_err = KafkaPublishError::Unavailable;

        while attempts < self.retry_policy.max_attempts {
            attempts += 1;
            match self
                .producer
                .publish(&self.topic, partition_key, &payload_json)
                .await
            {
                Ok(()) => {
                    if let Some(ref idem) = event.idempotency_key
                        && let Ok(mut map) = self.published_idempotency.lock()
                    {
                        map.insert(
                            idem.clone(),
                            (event.event_id.clone(), event.enrollment_id.clone()),
                        );
                    }
                    return Ok(event.event_id.clone());
                }
                Err(err) => {
                    last_err = err;
                    if attempts < self.retry_policy.max_attempts {
                        let base = self
                            .retry_policy
                            .base_delay_ms
                            .saturating_mul(
                                1u64.checked_shl(attempts.saturating_sub(1))
                                    .unwrap_or(u64::MAX),
                            )
                            .min(self.retry_policy.max_delay_ms);
                        let clock_jitter = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|duration| duration.subsec_nanos() as u64 % 31)
                            .unwrap_or(0);
                        let delay_ms = base
                            .saturating_add(clock_jitter)
                            .min(self.retry_policy.max_delay_ms);
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    }
                }
            }
        }

        Err(last_err)
    }
}
