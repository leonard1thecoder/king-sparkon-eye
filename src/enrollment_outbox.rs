//! Durable Eye enrollment persistence/outbox integration.
//!
//! PostgreSQL atomically stores enrollment metadata and publish intent. The dispatcher rechecks
//! authoritative Tracker consent immediately before publication and marks delivered only after
//! Kafka acknowledges the event.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use thiserror::Error;

use crate::config::EnrollmentRuntimeConfig;
use crate::contract::{PersistenceStatus, validate_identifier};
use crate::domain::events::{EnrollmentEventPublisher, KafkaPublishError, PhotoEnrolledEventV1};
use crate::domain::kafka_producer::RdkafkaKafkaProducer;
use crate::storage::{EnrollmentRecord, EnrollmentStatus};

const CONSENT_PURPOSE: &str = "EYE_PHOTO_ENROLLMENT";
const MAX_PUBLISH_ATTEMPTS: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum OutboxError {
    #[error("durable outbox is unavailable")]
    Unavailable,
    #[error("durable outbox rejected the operation")]
    Rejected,
    #[error("consent authority is unavailable")]
    ConsentUnavailable,
    #[error("consent was not verified")]
    ConsentDenied,
    #[error("enrollment is not eligible for durable event publication")]
    IneligibleEnrollment,
    #[error("enrollment idempotency conflict")]
    IdempotencyConflict,
    #[error("enrollment event failed contract validation")]
    InvalidEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsentDecision {
    Verified,
    Denied,
    Unavailable,
}

#[derive(Debug, Deserialize)]
struct ConsentResponse {
    verified: bool,
}

#[derive(Debug, Deserialize)]
struct EnqueueResponse {
    outcome: String,
    event_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BooleanResponse {
    ok: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ClaimedOutboxRow {
    pub event_id: String,
    pub enrollment_id: String,
    pub consent_reference: String,
    pub policy_version: String,
    pub payload: Value,
    pub attempts: u32,
}

#[derive(Debug, Deserialize)]
struct OutboxStateResponse {
    outcome: String,
}

#[derive(Clone)]
pub struct ConsentVerifier {
    client: Client,
    url: String,
    service_key: String,
}

impl ConsentVerifier {
    pub fn new(config: &EnrollmentRuntimeConfig) -> Result<Self, OutboxError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| OutboxError::ConsentUnavailable)?;
        Ok(Self {
            client,
            url: config.consent_verify_url.clone(),
            service_key: config.consent_service_key.expose().to_owned(),
        })
    }

    pub async fn verify(&self, consent_reference: &str, policy_version: &str) -> ConsentDecision {
        let response = self
            .client
            .post(&self.url)
            .header("X-Eye-Service-Key", &self.service_key)
            .json(&json!({
                "consentReference": consent_reference,
                "purpose": CONSENT_PURPOSE,
                "policyVersion": policy_version
            }))
            .send()
            .await;
        let Ok(response) = response else {
            return ConsentDecision::Unavailable;
        };
        if response.status() != StatusCode::OK
            || response
                .content_length()
                .is_some_and(|length| length > 8192)
        {
            return ConsentDecision::Unavailable;
        }
        let bytes = match response.bytes().await {
            Ok(bytes) if bytes.len() <= 8192 => bytes,
            _ => return ConsentDecision::Unavailable,
        };
        match serde_json::from_slice::<ConsentResponse>(&bytes) {
            Ok(result) if result.verified => ConsentDecision::Verified,
            Ok(_) => ConsentDecision::Denied,
            Err(_) => ConsentDecision::Unavailable,
        }
    }
}

#[derive(Clone)]
pub struct DurableEyeOutbox {
    client: Client,
    base_url: String,
    service_key: String,
}

impl DurableEyeOutbox {
    pub fn new(config: &EnrollmentRuntimeConfig) -> Result<Self, OutboxError> {
        let client = Client::builder()
            .timeout(Duration::from_secs(5))
            .connect_timeout(Duration::from_secs(3))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| OutboxError::Unavailable)?;
        Ok(Self {
            client,
            base_url: config.database_rest_url.trim_end_matches('/').to_owned(),
            service_key: config.database_service_role_key.expose().to_owned(),
        })
    }

    async fn rpc<T: DeserializeOwned>(
        &self,
        function: &str,
        body: Value,
        max_bytes: usize,
    ) -> Result<Vec<T>, OutboxError> {
        let response = self
            .client
            .post(format!("{}/rpc/{function}", self.base_url))
            .header("apikey", &self.service_key)
            .bearer_auth(&self.service_key)
            .timeout(Duration::from_secs(5))
            .json(&body)
            .send()
            .await
            .map_err(|_| OutboxError::Unavailable)?;
        if response.status() == StatusCode::UNAUTHORIZED
            || response.status() == StatusCode::FORBIDDEN
        {
            return Err(OutboxError::Rejected);
        }
        if !response.status().is_success()
            || response
                .content_length()
                .is_some_and(|length| length > max_bytes as u64)
        {
            return Err(OutboxError::Unavailable);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|_| OutboxError::Unavailable)?;
        if bytes.len() > max_bytes {
            return Err(OutboxError::InvalidEvent);
        }
        serde_json::from_slice(&bytes).map_err(|_| OutboxError::InvalidEvent)
    }

    pub async fn enqueue(
        &self,
        record: &EnrollmentRecord,
        event: &PhotoEnrolledEventV1,
        now_ms: u64,
    ) -> Result<String, OutboxError> {
        event.validate().map_err(|_| OutboxError::InvalidEvent)?;
        let payload = serde_json::to_value(event).map_err(|_| OutboxError::InvalidEvent)?;
        let rows: Vec<EnqueueResponse> = self
            .rpc(
                "eye_commit_enrollment_and_outbox",
                json!({
                    "p_enrollment_id": record.enrollment_id,
                    "p_event_id": event.event_id,
                    "p_consent_reference": record.consent_reference,
                    "p_object_key": record.object_key,
                    "p_content_sha256": record.content_sha256,
                    "p_byte_len": record.byte_len as u64,
                    "p_image_width": record.width,
                    "p_image_height": record.height,
                    "p_policy_version": record.policy_version,
                    "p_model_version": record.model_version,
                    "p_idempotency_key": record.idempotency_key,
                    "p_status": "STORED",
                    "p_persistence": "PENDING",
                    "p_occurred_at_ms": event.occurred_at_ms,
                    "p_payload": payload,
                    "p_now_ms": now_ms
                }),
                8192,
            )
            .await?;
        let result = rows.into_iter().next().ok_or(OutboxError::Unavailable)?;
        match result.outcome.as_str() {
            "created" | "existing" => result.event_id.ok_or(OutboxError::Unavailable),
            "conflict" => Err(OutboxError::IdempotencyConflict),
            _ => Err(OutboxError::Unavailable),
        }
    }

    pub async fn claim_due(
        &self,
        now_ms: u64,
        lease_ms: u64,
        batch_size: u32,
    ) -> Result<Vec<ClaimedOutboxRow>, OutboxError> {
        self.rpc(
            "eye_claim_due_outbox",
            json!({
                "p_now_ms": now_ms,
                "p_lease_ms": lease_ms,
                "p_batch_size": batch_size
            }),
            4 * 1024 * 1024,
        )
        .await
    }

    pub async fn mark_delivered(&self, event_id: &str, now_ms: u64) -> Result<(), OutboxError> {
        let rows: Vec<BooleanResponse> = self
            .rpc(
                "eye_mark_outbox_delivered",
                json!({"p_event_id": event_id, "p_now_ms": now_ms}),
                8192,
            )
            .await?;
        if rows.first().is_some_and(|row| row.ok) {
            Ok(())
        } else {
            Err(OutboxError::Unavailable)
        }
    }

    pub async fn reschedule(
        &self,
        event_id: &str,
        attempts: u32,
        error_category: &'static str,
        now_ms: u64,
    ) -> Result<(), OutboxError> {
        let exponent = attempts.saturating_sub(1).min(6);
        let backoff =
            (1_000u64.saturating_mul(1u64.checked_shl(exponent).unwrap_or(64))).min(60_000);
        let jitter = now_ms.wrapping_add(u64::from(attempts).wrapping_mul(7919)) % 701;
        let rows: Vec<OutboxStateResponse> = self
            .rpc(
                "eye_reschedule_outbox",
                json!({
                    "p_event_id": event_id,
                    "p_error_category": error_category,
                    "p_now_ms": now_ms,
                    "p_delay_ms": backoff + jitter,
                    "p_max_attempts": MAX_PUBLISH_ATTEMPTS
                }),
                8192,
            )
            .await?;
        if rows
            .first()
            .is_some_and(|row| matches!(row.outcome.as_str(), "pending" | "dead_letter"))
        {
            Ok(())
        } else {
            Err(OutboxError::Unavailable)
        }
    }

    pub async fn dead_letter(
        &self,
        event_id: &str,
        error_category: &'static str,
        now_ms: u64,
    ) -> Result<(), OutboxError> {
        let rows: Vec<BooleanResponse> = self
            .rpc(
                "eye_dead_letter_outbox",
                json!({
                    "p_event_id": event_id,
                    "p_error_category": error_category,
                    "p_now_ms": now_ms
                }),
                8192,
            )
            .await?;
        if rows.first().is_some_and(|row| row.ok) {
            Ok(())
        } else {
            Err(OutboxError::Unavailable)
        }
    }

    /// Intended only for a separately authenticated operations tool using the service-role key.
    pub async fn replay_dead_letter(
        &self,
        event_id: &str,
        operator_reference: &str,
        now_ms: u64,
    ) -> Result<(), OutboxError> {
        validate_identifier("operatorReference", operator_reference)
            .map_err(|_| OutboxError::Rejected)?;
        let rows: Vec<BooleanResponse> = self
            .rpc(
                "eye_replay_dead_letter",
                json!({
                    "p_event_id": event_id,
                    "p_operator_reference": operator_reference,
                    "p_now_ms": now_ms
                }),
                8192,
            )
            .await?;
        if rows.first().is_some_and(|row| row.ok) {
            Ok(())
        } else {
            Err(OutboxError::Rejected)
        }
    }
}

#[derive(Clone)]
pub struct DurableEnrollmentCoordinator {
    store: DurableEyeOutbox,
    consent: ConsentVerifier,
}

impl DurableEnrollmentCoordinator {
    pub fn new(store: DurableEyeOutbox, consent: ConsentVerifier) -> Self {
        Self { store, consent }
    }

    /// Call only after deterministic assessment passed, caller authorization succeeded, and the
    /// private object store returned a real Stored record. This is not an HTTP input method.
    pub async fn commit_stored_enrollment(
        &self,
        record: &EnrollmentRecord,
        event_id: &str,
    ) -> Result<String, OutboxError> {
        if record.status != EnrollmentStatus::Stored
            || record.persistence != PersistenceStatus::Pending
        {
            return Err(OutboxError::IneligibleEnrollment);
        }
        validate_identifier("eventId", event_id).map_err(|_| OutboxError::InvalidEvent)?;
        match self
            .consent
            .verify(&record.consent_reference, &record.policy_version)
            .await
        {
            ConsentDecision::Verified => {}
            ConsentDecision::Denied => return Err(OutboxError::ConsentDenied),
            ConsentDecision::Unavailable => return Err(OutboxError::ConsentUnavailable),
        }
        let event = PhotoEnrolledEventV1::from_record(record, event_id.to_owned(), now_ms())
            .map_err(|_| OutboxError::InvalidEvent)?;
        self.store.enqueue(record, &event, now_ms()).await
    }
}

pub struct OutboxRuntime {
    store: DurableEyeOutbox,
    consent: ConsentVerifier,
    publisher: EnrollmentEventPublisher<RdkafkaKafkaProducer>,
}

impl OutboxRuntime {
    pub fn new(config: &EnrollmentRuntimeConfig) -> Result<Self, OutboxError> {
        let store = DurableEyeOutbox::new(config)?;
        let consent = ConsentVerifier::new(config)?;
        let producer = RdkafkaKafkaProducer::new(config).map_err(|_| OutboxError::Unavailable)?;
        Ok(Self {
            store,
            consent,
            publisher: EnrollmentEventPublisher::new(producer),
        })
    }

    pub fn coordinator(&self) -> DurableEnrollmentCoordinator {
        DurableEnrollmentCoordinator::new(self.store.clone(), self.consent.clone())
    }

    pub async fn run(self) {
        loop {
            match self.store.claim_due(now_ms(), 300_000, 4).await {
                Ok(rows) if rows.is_empty() => tokio::time::sleep(Duration::from_millis(500)).await,
                Ok(rows) => {
                    for row in rows {
                        self.process_row(row).await;
                    }
                }
                Err(_) => {
                    tracing::warn!(
                        error_category = "OUTBOX_STORE_UNAVAILABLE",
                        "Eye outbox claim failed"
                    );
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            }
        }
    }

    async fn process_row(&self, row: ClaimedOutboxRow) {
        let event: PhotoEnrolledEventV1 = match serde_json::from_value(row.payload.clone()) {
            Ok(event)
                if event.validate().is_ok()
                    && event.event_id == row.event_id
                    && event.enrollment_id == row.enrollment_id
                    && event.consent_reference == row.consent_reference
                    && event.policy_version == row.policy_version =>
            {
                event
            }
            _ => {
                let _ = self
                    .store
                    .dead_letter(&row.event_id, "EVENT_CONTRACT_INVALID", now_ms())
                    .await;
                return;
            }
        };

        match self
            .consent
            .verify(&row.consent_reference, &row.policy_version)
            .await
        {
            ConsentDecision::Denied => {
                let _ = self
                    .store
                    .dead_letter(&row.event_id, "CONSENT_NOT_ACTIVE", now_ms())
                    .await;
                return;
            }
            ConsentDecision::Unavailable => {
                let _ = self
                    .store
                    .reschedule(
                        &row.event_id,
                        row.attempts,
                        "CONSENT_AUTHORITY_UNAVAILABLE",
                        now_ms(),
                    )
                    .await;
                return;
            }
            ConsentDecision::Verified => {}
        }

        match self.publisher.publish_enrollment_event(&event).await {
            Ok(acknowledged_event_id) if acknowledged_event_id == row.event_id => {
                if self
                    .store
                    .mark_delivered(&row.event_id, now_ms())
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        error_category = "OUTBOX_ACK_PERSIST_FAILED",
                        "Kafka acknowledged an Eye event but durable delivery status could not be updated"
                    );
                }
            }
            Ok(_) => {
                let _ = self
                    .store
                    .dead_letter(&row.event_id, "EVENT_ID_MISMATCH", now_ms())
                    .await;
            }
            Err(KafkaPublishError::ValidationError(_))
            | Err(KafkaPublishError::IneligibleEnrollment) => {
                let _ = self
                    .store
                    .dead_letter(&row.event_id, "EVENT_CONTRACT_INVALID", now_ms())
                    .await;
            }
            Err(_) => {
                let _ = self
                    .store
                    .reschedule(
                        &row.event_id,
                        row.attempts,
                        "KAFKA_DELIVERY_FAILED",
                        now_ms(),
                    )
                    .await;
            }
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
