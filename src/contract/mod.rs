//! Wire contract for buyer photo assessment and QR-free gate identification (issue #22).
//!
//! The OpenAPI document at `openapi/eye.openapi.json` is the source of truth for
//! external consumers. These types mirror it, and `tests/contract_test.rs` fails
//! when the two drift apart.
//!
//! Image suitability, persistence and face-match outcome are separate fields.
//! Nothing in this contract represents final admission. Only Spring Boot can
//! admit an attendee, after an authenticated worker approves entry.

use serde::{Deserialize, Serialize};

use crate::error::ValidationError;

/// Maximum length of any client- or session-supplied identifier.
pub const MAX_IDENTIFIER_LEN: usize = 128;

/// Outcome of the objective and supplementary image checks. Not an identity decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SuitabilityStatus {
    AssessmentPassed,
    RetakeRequired,
    ReviewRequired,
    /// Operational failure (for example, provider timeout). Distinct from a poor photo.
    AssessmentUnavailable,
}

impl SuitabilityStatus {
    pub const ALL: &'static [SuitabilityStatus] = &[
        SuitabilityStatus::AssessmentPassed,
        SuitabilityStatus::RetakeRequired,
        SuitabilityStatus::ReviewRequired,
        SuitabilityStatus::AssessmentUnavailable,
    ];
}

/// Progress of storing an accepted photo and its metadata. Rust reports PENDING
/// until Spring Boot confirms persistence, and Rust never reports CONFIRMED on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PersistenceStatus {
    NotRequested,
    Pending,
    Confirmed,
    Failed,
}

impl PersistenceStatus {
    pub const ALL: &'static [PersistenceStatus] = &[
        PersistenceStatus::NotRequested,
        PersistenceStatus::Pending,
        PersistenceStatus::Confirmed,
        PersistenceStatus::Failed,
    ];
}

/// Result of event-scoped 1:N identification. A candidate is not an admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum MatchOutcome {
    LikelyMatch,
    Uncertain,
    LikelyMismatch,
    NoCandidate,
    Unavailable,
}

impl MatchOutcome {
    pub const ALL: &'static [MatchOutcome] = &[
        MatchOutcome::LikelyMatch,
        MatchOutcome::Uncertain,
        MatchOutcome::LikelyMismatch,
        MatchOutcome::NoCandidate,
        MatchOutcome::Unavailable,
    ];
}

/// Allowlisted reason codes, finalised in issue #26. Each code is produced by the deterministic
/// path, or is reserved until its check exists. `LowResolution` and `FaceNotFrontal` are reserved
/// because no current check measures them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ReasonCode {
    NoFaceDetected,
    MultipleFaces,
    FaceTooSmall,
    FaceTooLarge,
    FaceOutOfFrame,
    FaceNotFrontal,
    PoorLighting,
    LowResolution,
    ImageBlurry,
}

impl ReasonCode {
    pub const ALL: &'static [ReasonCode] = &[
        ReasonCode::NoFaceDetected,
        ReasonCode::MultipleFaces,
        ReasonCode::FaceTooSmall,
        ReasonCode::FaceTooLarge,
        ReasonCode::FaceOutOfFrame,
        ReasonCode::FaceNotFrontal,
        ReasonCode::PoorLighting,
        ReasonCode::LowResolution,
        ReasonCode::ImageBlurry,
    ];
}

/// Checks an identifier supplied by a client or issued by a server-side session.
/// Allowed characters are ASCII letters, digits, `-`, `_` and `.`.
pub fn validate_identifier(field: &'static str, value: &str) -> Result<(), ValidationError> {
    if value.is_empty() {
        return Err(ValidationError {
            field,
            rule: "must not be empty",
        });
    }
    if value.len() > MAX_IDENTIFIER_LEN {
        return Err(ValidationError {
            field,
            rule: "exceeds the maximum length",
        });
    }
    if !value
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        return Err(ValidationError {
            field,
            rule: "contains disallowed characters",
        });
    }
    Ok(())
}

fn validate_optional_identifier(
    field: &'static str,
    value: Option<&str>,
) -> Result<(), ValidationError> {
    match value {
        Some(v) => validate_identifier(field, v),
        None => Ok(()),
    }
}

/// Metadata sent alongside the image in `POST /v1/enrollment/photo-assessments`.
/// The image itself is a multipart part and is validated by the upload pipeline (issue #23).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoAssessmentRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_request_id: Option<String>,
}

impl PhotoAssessmentRequest {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_optional_identifier("clientRequestId", self.client_request_id.as_deref())
    }
}

/// Response for a buyer photo assessment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoAssessmentResponse {
    pub assessment_id: String,
    pub request_id: String,
    pub suitability: SuitabilityStatus,
    pub reason_codes: Vec<ReasonCode>,
    pub persistence: PersistenceStatus,
    /// Frontier model version used for supplementary guidance; null when the provider is disabled.
    pub model_version: Option<String>,
    pub policy_version: String,
}

/// Metadata for `POST /v1/gate/identifications`. The primary gate flow sends no ticket QR
/// data and no ticket ID. The server-issued, event-scoped gate session identifies the context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateIdentificationRequest {
    pub gate_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_request_id: Option<String>,
}

impl GateIdentificationRequest {
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_identifier("gateSessionId", &self.gate_session_id)?;
        validate_optional_identifier("clientRequestId", self.client_request_id.as_deref())
    }
}

/// Response for a gate identification. It intentionally has no admission field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GateIdentificationResponse {
    pub request_id: String,
    pub gate_session_id: String,
    pub outcome: MatchOutcome,
    pub reason_codes: Vec<ReasonCode>,
    pub recognizer_version: String,
    pub policy_version: String,
}
