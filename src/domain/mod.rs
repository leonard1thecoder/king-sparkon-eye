//! Assessment orchestration (issue #26).
//!
//! Combines the deterministic path (face detection, objective quality checks) with the
//! optional frontier advisory (issue #25). The deterministic path is authoritative. The
//! advisory can add guidance and report its own status, but it cannot change suitability
//! or reason codes.
//!
//! Precedence, highest first:
//! 1. Detector runtime or output failure: `ASSESSMENT_UNAVAILABLE`, with no reason codes.
//! 2. Zero faces: `RETAKE_REQUIRED` with `NO_FACE_DETECTED`.
//! 3. Several faces: `RETAKE_REQUIRED` with `MULTIPLE_FACES`.
//! 4. One face with any measurement `Unknown`: `ASSESSMENT_UNAVAILABLE`. An unknown value is never a pass.
//! 5. One face with failing checks: `RETAKE_REQUIRED` with one reason code per failing check.
//! 6. One face with every check passing: `ASSESSMENT_PASSED`.
//!
//! Invalid uploads never reach this module. Ingress rejects them first, so no detector or
//! provider call happens.
//!
//! Provider outages are reported as [`AdvisoryStatus::Unavailable`]. They never turn a
//! poor photo into a failure or a good photo into a pass, unless [`AdvisoryPolicy::RequireAdvisory`]
//! is configured. In that case a pass also needs a validated advisory.

pub mod events;
pub use events::*;

use crate::advisory::{self, Advisory, AdvisoryOutcomeError, FinalAssessment};
use crate::contract::{ReasonCode, SuitabilityStatus};
use crate::inference::yunet::DEFAULT_SCORE_THRESHOLD;
use crate::inference::{DetectionOutcome, FaceDetector, InferenceError, classify};
use crate::ingress::NormalizedImage;
use crate::provider::{ProviderError, VisionProvider, encode_png};
use crate::quality::{QualityReport, QualityThresholds, Verdict, measure};

/// Whether a provider outage can still allow a pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdvisoryPolicy {
    /// Default. Deterministic results stand alone. A provider outage or rejected output
    /// leaves guidance empty and never changes suitability.
    #[default]
    PassWithoutAdvisory,
    /// A pass also needs validated advisory output. When the advisory is enabled but
    /// unavailable or rejected, a pass becomes `ASSESSMENT_UNAVAILABLE`. Retake results are
    /// unchanged. This policy has no effect while the advisory is disabled.
    RequireAdvisory,
}

/// Settings for one assessment.
#[derive(Debug, Clone, PartialEq)]
pub struct AssessmentConfig {
    /// Detections below this score are dropped before counting faces. Defaults to the detector's own threshold.
    pub min_detection_score: f32,
    /// Objective check thresholds. Uncalibrated until evaluated (see issue #8).
    pub thresholds: QualityThresholds,
    /// Whether the frontier advisory (issue #25) is called.
    pub advisory_enabled: bool,
    pub advisory_policy: AdvisoryPolicy,
}

impl Default for AssessmentConfig {
    fn default() -> Self {
        Self {
            min_detection_score: DEFAULT_SCORE_THRESHOLD,
            thresholds: QualityThresholds::default(),
            advisory_enabled: false,
            advisory_policy: AdvisoryPolicy::default(),
        }
    }
}

/// Result of the deterministic path alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Deterministic {
    pub suitability: SuitabilityStatus,
    pub reason_codes: Vec<ReasonCode>,
}

impl Deterministic {
    fn unavailable() -> Self {
        Self {
            suitability: SuitabilityStatus::AssessmentUnavailable,
            reason_codes: Vec::new(),
        }
    }

    fn retake(mut reason_codes: Vec<ReasonCode>) -> Self {
        reason_codes.sort();
        reason_codes.dedup();
        Self {
            suitability: SuitabilityStatus::RetakeRequired,
            reason_codes,
        }
    }
}

/// Applies the precedence table to structured inputs. It is a pure function, so identical inputs always give the same output.
///
/// `quality` must be `Some` when `detection` is [`DetectionOutcome::OneFace`], and `None` otherwise.
pub fn decide(
    detection: &Result<DetectionOutcome, InferenceError>,
    quality: Option<&QualityReport>,
    thresholds: &QualityThresholds,
) -> Deterministic {
    let outcome = match detection {
        Ok(outcome) => outcome,
        Err(_) => return Deterministic::unavailable(),
    };
    match outcome {
        DetectionOutcome::NoFace => Deterministic::retake(vec![ReasonCode::NoFaceDetected]),
        DetectionOutcome::MultipleFaces { .. } => {
            Deterministic::retake(vec![ReasonCode::MultipleFaces])
        }
        DetectionOutcome::OneFace(_) => {
            let Some(q) = quality else {
                return Deterministic::unavailable();
            };
            match failing_reasons(q, thresholds) {
                None => Deterministic::unavailable(),
                Some(reasons) if reasons.is_empty() => Deterministic {
                    suitability: SuitabilityStatus::AssessmentPassed,
                    reason_codes: Vec::new(),
                },
                Some(reasons) => Deterministic::retake(reasons),
            }
        }
    }
}

/// One reason code per failing check. Returns `None` when any check is unknown.
fn failing_reasons(q: &QualityReport, t: &QualityThresholds) -> Option<Vec<ReasonCode>> {
    let verdicts = [q.sharpness, q.exposure, q.face_size, q.framing];
    if verdicts.contains(&Verdict::Unknown) {
        return None;
    }
    let mut reasons = Vec::new();
    if q.sharpness == Verdict::Fail {
        reasons.push(ReasonCode::ImageBlurry);
    }
    if q.exposure == Verdict::Fail {
        reasons.push(ReasonCode::PoorLighting);
    }
    if q.face_size == Verdict::Fail {
        // Size is only Fail outside the accepted range. Its direction picks the code.
        let width = q.face_width_fraction?;
        if width < t.min_face_width_fraction {
            reasons.push(ReasonCode::FaceTooSmall);
        } else {
            reasons.push(ReasonCode::FaceTooLarge);
        }
    }
    if q.framing == Verdict::Fail {
        reasons.push(ReasonCode::FaceOutOfFrame);
    }
    Some(reasons)
}

/// Calls the provider once and validates its output. The adapter enforces its own timeout and retry limits.
async fn ask_advisory<P: VisionProvider>(
    image: &NormalizedImage,
    provider: &P,
) -> Result<Advisory, AdvisoryOutcomeError> {
    let png = encode_png(image).map_err(AdvisoryOutcomeError::Provider)?;
    let text = provider.advise(png).await;
    advisory::from_provider_text(&text)
}

/// Runs the full assessment for one normalised image.
///
/// The image must already have passed ingress. Every failure becomes a typed outcome in the
/// result, so there is no error path for callers to misread.
pub async fn assess<D: FaceDetector, P: VisionProvider>(
    image: &NormalizedImage,
    detector: &D,
    provider: &P,
    config: &AssessmentConfig,
) -> FinalAssessment {
    let detection = detector
        .detect(image)
        .map(|detections| classify(detections, config.min_detection_score));

    let quality = match &detection {
        Ok(DetectionOutcome::OneFace(face)) => Some(measure(image, Some(face), &config.thresholds)),
        _ => None,
    };
    let deterministic = decide(&detection, quality.as_ref(), &config.thresholds);

    let advisory_outcome = if config.advisory_enabled {
        ask_advisory(image, provider).await
    } else {
        // Not called while disabled. `finalise` ignores this value when `enabled` is false.
        Err(AdvisoryOutcomeError::Provider(ProviderError::Disabled))
    };

    let mut result = advisory::finalise(
        deterministic.suitability,
        &deterministic.reason_codes,
        advisory_outcome,
        config.advisory_enabled,
    );

    // Only an explicit policy can turn a pass into unavailable because of the advisory.
    // Retake and unavailable results are unchanged.
    if config.advisory_enabled
        && config.advisory_policy == AdvisoryPolicy::RequireAdvisory
        && result.suitability == SuitabilityStatus::AssessmentPassed
        && !matches!(result.advisory, advisory::AdvisoryStatus::Used)
    {
        result.suitability = SuitabilityStatus::AssessmentUnavailable;
        result.reason_codes.clear();
        result.guidance.clear();
    }
    result
}

/// Applies the conservative MVP calibration gate. Current quality thresholds are starting
/// points only; until evaluated against a consented dataset, a nominal pass is unavailable.
/// Explicit retake outcomes and detector failures retain their existing typed outcomes.
pub fn conservative_result(mut result: FinalAssessment) -> FinalAssessment {
    if result.suitability == SuitabilityStatus::AssessmentPassed {
        result.suitability = SuitabilityStatus::AssessmentUnavailable;
        result.reason_codes.clear();
        result.guidance.clear();
    }
    result
}

#[cfg(test)]
mod calibration_gate_tests {
    use super::*;
    use crate::advisory::AdvisoryStatus;

    #[test]
    fn uncalibrated_pass_is_reported_as_unavailable() {
        let result = FinalAssessment {
            suitability: SuitabilityStatus::AssessmentPassed,
            reason_codes: Vec::new(),
            guidance: Vec::new(),
            advisory: AdvisoryStatus::Disabled,
        };
        let result = conservative_result(result);
        assert_eq!(result.suitability, SuitabilityStatus::AssessmentUnavailable);
    }

    #[test]
    fn retake_result_is_preserved_by_calibration_gate() {
        let result = FinalAssessment {
            suitability: SuitabilityStatus::RetakeRequired,
            reason_codes: vec![ReasonCode::NoFaceDetected],
            guidance: Vec::new(),
            advisory: AdvisoryStatus::Disabled,
        };
        let result = conservative_result(result);
        assert_eq!(result.suitability, SuitabilityStatus::RetakeRequired);
        assert_eq!(result.reason_codes, vec![ReasonCode::NoFaceDetected]);
    }
}
