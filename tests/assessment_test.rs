//! Assessment orchestration tests (issue #26). All inputs are in-memory fixtures and fakes.
//! No network, no real model and no real face images are used.

use king_sparkon_eye::advisory::{AdvisoryStatus, GuidanceCode};
use king_sparkon_eye::contract::{ReasonCode, SuitabilityStatus};
use king_sparkon_eye::domain::{AdvisoryPolicy, AssessmentConfig, assess, decide};
use king_sparkon_eye::inference::{
    DetectionOutcome, FaceDetection, FaceDetector, InferenceError, NormalizedBox,
};
use king_sparkon_eye::ingress::{NormalizedImage, SupportedFormat};
use king_sparkon_eye::provider::{MockProvider, ProviderError};
use king_sparkon_eye::quality::{QualityReport, QualityThresholds, Verdict};

const VALID_GOOD: &str =
    r#"{"framing":"good","lighting":"good","face_visible":"clear","retake_may_help":false}"#;
const VALID_TOO_DARK: &str =
    r#"{"framing":"good","lighting":"too_dark","face_visible":"clear","retake_may_help":true}"#;

// ---------- fixtures ----------

/// Alternating 60 and 180 per pixel: strong edges, mean 120, no clipping. It passes sharpness and exposure.
fn sharp_image(width: u32, height: u32) -> NormalizedImage {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let v = if (x + y) % 2 == 0 { 60 } else { 180 };
            rgb.extend_from_slice(&[v, v, v]);
        }
    }
    NormalizedImage {
        format: SupportedFormat::Png,
        width,
        height,
        rgb,
    }
}

fn face(x0: f32, y0: f32, x1: f32, y1: f32, score: f32) -> FaceDetection {
    FaceDetection {
        bbox: NormalizedBox::new(x0, y0, x1, y1).unwrap(),
        score,
        landmarks: Vec::new(),
    }
}

/// A face that passes every check: width 0.4 and inside the frame margin.
fn good_face() -> FaceDetection {
    face(0.3, 0.3, 0.7, 0.8, 0.95)
}

struct FakeDetector {
    result: Result<Vec<FaceDetection>, InferenceError>,
}

impl FaceDetector for FakeDetector {
    fn detect(&self, _image: &NormalizedImage) -> Result<Vec<FaceDetection>, InferenceError> {
        self.result.clone()
    }
}

fn detector(faces: Vec<FaceDetection>) -> FakeDetector {
    FakeDetector { result: Ok(faces) }
}

fn provider_ok(text: &str) -> MockProvider {
    MockProvider {
        response: Ok(text.to_string()),
    }
}

fn provider_err(e: ProviderError) -> MockProvider {
    MockProvider { response: Err(e) }
}

fn advisory_on() -> AssessmentConfig {
    AssessmentConfig {
        advisory_enabled: true,
        ..AssessmentConfig::default()
    }
}

/// A quality report where every check passes. Override fields in the tests as needed.
fn passing_report(width: f32) -> QualityReport {
    QualityReport {
        sharpness: Verdict::Pass,
        sharpness_value: Some(500.0),
        exposure: Verdict::Pass,
        mean_luma: Some(120.0),
        clipped_fraction: Some(0.0),
        face_size: Verdict::Pass,
        face_width_fraction: Some(width),
        framing: Verdict::Pass,
    }
}

/// One row of the precedence table: a name, the inputs, and the expected outcome.
type Case = (
    &'static str,
    Result<DetectionOutcome, InferenceError>,
    Option<QualityReport>,
    SuitabilityStatus,
    Vec<ReasonCode>,
);

fn one_face() -> DetectionOutcome {
    DetectionOutcome::OneFace(good_face())
}

// ---------- precedence table (decide) ----------

#[test]
fn precedence_table_maps_each_input_to_one_outcome() {
    let t = QualityThresholds::default();

    // (name, detection, quality, expected suitability, expected reasons)
    let cases: Vec<Case> = vec![
        (
            "detector runtime failure is unavailable, not a retake",
            Err(InferenceError::Runtime),
            None,
            SuitabilityStatus::AssessmentUnavailable,
            vec![],
        ),
        (
            "detector output shape error is unavailable",
            Err(InferenceError::UnexpectedShape),
            None,
            SuitabilityStatus::AssessmentUnavailable,
            vec![],
        ),
        (
            "missing model artifact is unavailable",
            Err(InferenceError::ArtifactMissing),
            None,
            SuitabilityStatus::AssessmentUnavailable,
            vec![],
        ),
        (
            "no face is a retake",
            Ok(DetectionOutcome::NoFace),
            None,
            SuitabilityStatus::RetakeRequired,
            vec![ReasonCode::NoFaceDetected],
        ),
        (
            "several faces is a retake",
            Ok(DetectionOutcome::MultipleFaces { count: 3 }),
            None,
            SuitabilityStatus::RetakeRequired,
            vec![ReasonCode::MultipleFaces],
        ),
        (
            "one face, every check passes",
            Ok(one_face()),
            Some(passing_report(0.4)),
            SuitabilityStatus::AssessmentPassed,
            vec![],
        ),
        (
            "one face, blurry",
            Ok(one_face()),
            Some(QualityReport {
                sharpness: Verdict::Fail,
                ..passing_report(0.4)
            }),
            SuitabilityStatus::RetakeRequired,
            vec![ReasonCode::ImageBlurry],
        ),
        (
            "one face, bad exposure",
            Ok(one_face()),
            Some(QualityReport {
                exposure: Verdict::Fail,
                ..passing_report(0.4)
            }),
            SuitabilityStatus::RetakeRequired,
            vec![ReasonCode::PoorLighting],
        ),
        (
            "one face, too small",
            Ok(one_face()),
            Some(QualityReport {
                face_size: Verdict::Fail,
                ..passing_report(0.10)
            }),
            SuitabilityStatus::RetakeRequired,
            vec![ReasonCode::FaceTooSmall],
        ),
        (
            "one face, too large",
            Ok(one_face()),
            Some(QualityReport {
                face_size: Verdict::Fail,
                ..passing_report(0.95)
            }),
            SuitabilityStatus::RetakeRequired,
            vec![ReasonCode::FaceTooLarge],
        ),
        (
            "one face, out of frame",
            Ok(one_face()),
            Some(QualityReport {
                framing: Verdict::Fail,
                ..passing_report(0.4)
            }),
            SuitabilityStatus::RetakeRequired,
            vec![ReasonCode::FaceOutOfFrame],
        ),
        (
            "several failing checks give one code each, sorted",
            Ok(one_face()),
            Some(QualityReport {
                sharpness: Verdict::Fail,
                exposure: Verdict::Fail,
                framing: Verdict::Fail,
                ..passing_report(0.4)
            }),
            SuitabilityStatus::RetakeRequired,
            vec![
                ReasonCode::FaceOutOfFrame,
                ReasonCode::PoorLighting,
                ReasonCode::ImageBlurry,
            ],
        ),
        (
            "unknown sharpness is unavailable, never a pass",
            Ok(one_face()),
            Some(QualityReport {
                sharpness: Verdict::Unknown,
                ..passing_report(0.4)
            }),
            SuitabilityStatus::AssessmentUnavailable,
            vec![],
        ),
        (
            "unknown framing is unavailable, never a pass",
            Ok(one_face()),
            Some(QualityReport {
                framing: Verdict::Unknown,
                ..passing_report(0.4)
            }),
            SuitabilityStatus::AssessmentUnavailable,
            vec![],
        ),
        (
            "one face with no measurements is unavailable",
            Ok(one_face()),
            None,
            SuitabilityStatus::AssessmentUnavailable,
            vec![],
        ),
    ];

    for (name, detection, quality, suitability, reasons) in cases {
        let got = decide(&detection, quality.as_ref(), &t);
        assert_eq!(got.suitability, suitability, "suitability: {name}");
        assert_eq!(got.reason_codes, reasons, "reasons: {name}");
    }
}

#[test]
fn decide_is_deterministic_for_identical_inputs() {
    let t = QualityThresholds::default();
    let detection = Ok(one_face());
    let q = QualityReport {
        exposure: Verdict::Fail,
        ..passing_report(0.4)
    };
    let first = decide(&detection, Some(&q), &t);
    for _ in 0..50 {
        assert_eq!(decide(&detection, Some(&q), &t), first);
    }
}

// ---------- end-to-end with fakes ----------

#[tokio::test]
async fn good_photo_passes_with_advisory_disabled() {
    let result = assess(
        &sharp_image(200, 200),
        &detector(vec![good_face()]),
        &provider_err(ProviderError::Disabled),
        &AssessmentConfig::default(),
    )
    .await;
    assert_eq!(result.suitability, SuitabilityStatus::AssessmentPassed);
    assert!(result.reason_codes.is_empty());
    assert_eq!(result.advisory, AdvisoryStatus::Disabled);
}

#[tokio::test]
async fn no_face_is_a_retake_even_with_a_perfect_advisory() {
    let result = assess(
        &sharp_image(200, 200),
        &detector(vec![]),
        &provider_ok(VALID_GOOD),
        &advisory_on(),
    )
    .await;
    assert_eq!(result.suitability, SuitabilityStatus::RetakeRequired);
    assert_eq!(result.reason_codes, vec![ReasonCode::NoFaceDetected]);
    assert_eq!(result.advisory, AdvisoryStatus::Used);
}

#[tokio::test]
async fn low_score_detections_are_dropped_before_counting() {
    // The only detection is below the minimum score, so the photo has no face.
    let weak = face(0.3, 0.3, 0.7, 0.8, 0.1);
    let result = assess(
        &sharp_image(200, 200),
        &detector(vec![weak]),
        &provider_err(ProviderError::Disabled),
        &AssessmentConfig::default(),
    )
    .await;
    assert_eq!(result.reason_codes, vec![ReasonCode::NoFaceDetected]);
}

#[tokio::test]
async fn detector_failure_is_unavailable_even_when_the_advisory_is_good() {
    let failing = FakeDetector {
        result: Err(InferenceError::Runtime),
    };
    let result = assess(
        &sharp_image(200, 200),
        &failing,
        &provider_ok(VALID_GOOD),
        &advisory_on(),
    )
    .await;
    assert_eq!(result.suitability, SuitabilityStatus::AssessmentUnavailable);
    assert!(result.reason_codes.is_empty());
}

// ---------- advisory cannot change deterministic results ----------

#[tokio::test]
async fn advisory_cannot_turn_a_retake_into_a_pass() {
    // Blank image: the detector finds no face, so the deterministic result is a retake.
    // A perfect advisory must not change that.
    let result = assess(
        &sharp_image(200, 200),
        &detector(vec![]),
        &provider_ok(VALID_GOOD),
        &advisory_on(),
    )
    .await;
    assert_ne!(result.suitability, SuitabilityStatus::AssessmentPassed);
    assert_eq!(result.reason_codes, vec![ReasonCode::NoFaceDetected]);
}

#[tokio::test]
async fn advisory_cannot_remove_or_add_reason_codes() {
    // The face is blurry on a flat image. The advisory claims the lighting is too dark.
    // The reason codes must still come only from the deterministic checks.
    let flat = NormalizedImage {
        format: SupportedFormat::Png,
        width: 200,
        height: 200,
        rgb: vec![120; 200 * 200 * 3],
    };
    let result = assess(
        &flat,
        &detector(vec![good_face()]),
        &provider_ok(VALID_TOO_DARK),
        &advisory_on(),
    )
    .await;
    assert_eq!(result.suitability, SuitabilityStatus::RetakeRequired);
    assert_eq!(result.reason_codes, vec![ReasonCode::ImageBlurry]);
    // The advisory's guidance is still reported, as a fixed code.
    assert!(result.guidance.contains(&GuidanceCode::ImproveLighting));
}

// ---------- provider outage is not a poor-photo verdict ----------

#[tokio::test]
async fn provider_outage_keeps_a_passing_photo_passing_by_default() {
    let result = assess(
        &sharp_image(200, 200),
        &detector(vec![good_face()]),
        &provider_err(ProviderError::Timeout),
        &advisory_on(),
    )
    .await;
    assert_eq!(result.suitability, SuitabilityStatus::AssessmentPassed);
    assert!(result.guidance.is_empty());
    assert_eq!(
        result.advisory,
        AdvisoryStatus::Unavailable(ProviderError::Timeout)
    );
}

#[tokio::test]
async fn provider_outage_on_a_poor_photo_is_still_a_retake_with_its_reason() {
    let flat = NormalizedImage {
        format: SupportedFormat::Png,
        width: 200,
        height: 200,
        rgb: vec![120; 200 * 200 * 3],
    };
    let result = assess(
        &flat,
        &detector(vec![good_face()]),
        &provider_err(ProviderError::RateLimited),
        &advisory_on(),
    )
    .await;
    assert_eq!(result.suitability, SuitabilityStatus::RetakeRequired);
    assert_eq!(result.reason_codes, vec![ReasonCode::ImageBlurry]);
    assert_eq!(
        result.advisory,
        AdvisoryStatus::Unavailable(ProviderError::RateLimited)
    );
}

#[tokio::test]
async fn rejected_advisory_output_does_not_change_a_passing_photo() {
    let result = assess(
        &sharp_image(200, 200),
        &detector(vec![good_face()]),
        &provider_ok("not json at all"),
        &advisory_on(),
    )
    .await;
    assert_eq!(result.suitability, SuitabilityStatus::AssessmentPassed);
    assert!(matches!(result.advisory, AdvisoryStatus::Rejected(_)));
}

// ---------- degraded-mode policy ----------

#[tokio::test]
async fn require_advisory_policy_turns_an_outage_pass_into_unavailable() {
    let config = AssessmentConfig {
        advisory_enabled: true,
        advisory_policy: AdvisoryPolicy::RequireAdvisory,
        ..AssessmentConfig::default()
    };
    let result = assess(
        &sharp_image(200, 200),
        &detector(vec![good_face()]),
        &provider_err(ProviderError::Unavailable),
        &config,
    )
    .await;
    assert_eq!(result.suitability, SuitabilityStatus::AssessmentUnavailable);
    assert!(result.reason_codes.is_empty());
    assert!(result.guidance.is_empty());
}

#[tokio::test]
async fn require_advisory_policy_still_passes_with_a_validated_advisory() {
    let config = AssessmentConfig {
        advisory_enabled: true,
        advisory_policy: AdvisoryPolicy::RequireAdvisory,
        ..AssessmentConfig::default()
    };
    let result = assess(
        &sharp_image(200, 200),
        &detector(vec![good_face()]),
        &provider_ok(VALID_GOOD),
        &config,
    )
    .await;
    assert_eq!(result.suitability, SuitabilityStatus::AssessmentPassed);
}

#[tokio::test]
async fn require_advisory_policy_does_not_change_retakes() {
    let config = AssessmentConfig {
        advisory_enabled: true,
        advisory_policy: AdvisoryPolicy::RequireAdvisory,
        ..AssessmentConfig::default()
    };
    let result = assess(
        &sharp_image(200, 200),
        &detector(vec![]),
        &provider_err(ProviderError::Timeout),
        &config,
    )
    .await;
    assert_eq!(result.suitability, SuitabilityStatus::RetakeRequired);
    assert_eq!(result.reason_codes, vec![ReasonCode::NoFaceDetected]);
}

#[tokio::test]
async fn require_advisory_policy_has_no_effect_while_advisory_is_disabled() {
    let config = AssessmentConfig {
        advisory_enabled: false,
        advisory_policy: AdvisoryPolicy::RequireAdvisory,
        ..AssessmentConfig::default()
    };
    let result = assess(
        &sharp_image(200, 200),
        &detector(vec![good_face()]),
        &provider_err(ProviderError::Disabled),
        &config,
    )
    .await;
    assert_eq!(result.suitability, SuitabilityStatus::AssessmentPassed);
}

#[tokio::test]
async fn default_policy_is_pass_without_advisory() {
    assert_eq!(
        AdvisoryPolicy::default(),
        AdvisoryPolicy::PassWithoutAdvisory
    );
    assert_eq!(
        AssessmentConfig::default().advisory_policy,
        AdvisoryPolicy::PassWithoutAdvisory
    );
}

// ---------- determinism of the full pipeline ----------

#[tokio::test]
async fn identical_inputs_give_identical_outputs() {
    let image = sharp_image(200, 200);
    let det = detector(vec![good_face()]);
    let provider = provider_ok(VALID_TOO_DARK);
    let config = advisory_on();
    let first = assess(&image, &det, &provider, &config).await;
    for _ in 0..10 {
        assert_eq!(assess(&image, &det, &provider, &config).await, first);
    }
}
