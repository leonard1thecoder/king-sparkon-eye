//! Face classification and objective quality tests. Fixtures are built in memory from known pixels.

use king_sparkon_eye::inference::{
    DetectionOutcome, FaceDetection, FaceDetector, InferenceError, NormalizedBox, classify,
};
use king_sparkon_eye::ingress::{NormalizedImage, SupportedFormat};
use king_sparkon_eye::quality::{QualityThresholds, Verdict, exposure, measure, sharpness};

fn image(width: u32, height: u32, value: impl Fn(u32, u32) -> u8) -> NormalizedImage {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            let v = value(x, y);
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

fn flat(width: u32, height: u32, v: u8) -> NormalizedImage {
    image(width, height, |_, _| v)
}

/// Alternating 60 and 180 per pixel: strong edges, mean 120, no clipping.
fn checker(width: u32, height: u32) -> NormalizedImage {
    image(
        width,
        height,
        |x, y| if (x + y) % 2 == 0 { 60 } else { 180 },
    )
}

fn face(x0: f32, y0: f32, x1: f32, y1: f32) -> FaceDetection {
    FaceDetection {
        bbox: NormalizedBox::new(x0, y0, x1, y1).unwrap(),
        score: 0.99,
        landmarks: vec![],
    }
}

fn centred_face() -> FaceDetection {
    face(0.3, 0.2, 0.7, 0.8) // width 0.4: inside the size range and the frame margin
}

// ---- sharpness ----

#[test]
fn flat_image_is_not_sharp() {
    assert_eq!(sharpness(&flat(16, 16, 128)), Some(0.0));
}

#[test]
fn checkerboard_is_sharp() {
    assert!(sharpness(&checker(16, 16)).unwrap() > 100.0);
}

#[test]
fn sharpness_is_unknown_for_tiny_images_and_reports_unknown_verdict() {
    let tiny = flat(2, 2, 128);
    assert_eq!(sharpness(&tiny), None);
    let report = measure(&tiny, None, &QualityThresholds::default());
    assert_eq!(
        report.sharpness,
        Verdict::Unknown,
        "unmeasurable must not be a pass"
    );
}

#[test]
fn sharpness_threshold_is_inclusive() {
    let img = checker(16, 16);
    let value = sharpness(&img).unwrap();
    let at = QualityThresholds {
        min_sharpness: value,
        ..QualityThresholds::default()
    };
    let above = QualityThresholds {
        min_sharpness: value + 1.0,
        ..QualityThresholds::default()
    };
    assert_eq!(measure(&img, None, &at).sharpness, Verdict::Pass);
    assert_eq!(measure(&img, None, &above).sharpness, Verdict::Fail);
}

// ---- exposure ----

#[test]
fn balanced_exposure_passes() {
    let report = measure(&checker(16, 16), None, &QualityThresholds::default());
    assert_eq!(report.exposure, Verdict::Pass);
    assert!((report.mean_luma.unwrap() - 120.0).abs() < 1.0);
    assert_eq!(report.clipped_fraction, Some(0.0));
}

#[test]
fn underexposed_image_fails() {
    let report = measure(&flat(16, 16, 10), None, &QualityThresholds::default());
    assert_eq!(report.exposure, Verdict::Fail);
}

#[test]
fn overexposed_image_fails_on_clipping() {
    let report = measure(&flat(16, 16, 255), None, &QualityThresholds::default());
    assert_eq!(report.exposure, Verdict::Fail);
    assert_eq!(report.clipped_fraction, Some(1.0));
}

#[test]
fn mean_luma_boundary_is_inclusive() {
    let img = flat(8, 8, 100);
    let (mean, _) = exposure(&img).unwrap();
    let at = QualityThresholds {
        min_mean_luma: mean,
        ..QualityThresholds::default()
    };
    let just_above = QualityThresholds {
        min_mean_luma: mean + 0.5,
        ..QualityThresholds::default()
    };
    assert_eq!(measure(&img, None, &at).exposure, Verdict::Pass);
    assert_eq!(measure(&img, None, &just_above).exposure, Verdict::Fail);
}

#[test]
fn empty_image_exposure_is_unknown() {
    let empty = NormalizedImage {
        format: SupportedFormat::Jpeg,
        width: 0,
        height: 0,
        rgb: vec![],
    };
    let report = measure(&empty, None, &QualityThresholds::default());
    assert_eq!(report.exposure, Verdict::Unknown);
    assert_eq!(report.mean_luma, None);
}

// ---- face size and framing ----

#[test]
fn no_face_makes_face_checks_unknown_not_pass() {
    let report = measure(&checker(16, 16), None, &QualityThresholds::default());
    assert_eq!(report.face_size, Verdict::Unknown);
    assert_eq!(report.framing, Verdict::Unknown);
    assert_eq!(report.face_width_fraction, None);
}

#[test]
fn centred_face_of_good_size_passes_both_face_checks() {
    let report = measure(
        &checker(16, 16),
        Some(&centred_face()),
        &QualityThresholds::default(),
    );
    assert_eq!(report.face_size, Verdict::Pass);
    assert_eq!(report.framing, Verdict::Pass);
}

#[test]
fn face_width_boundaries_are_inclusive() {
    // Boundaries are chosen so the box width is exact in f32. Decimal values like 0.2 or 0.45
    // are not exact, so a width computed from them can fall just below a threshold of 0.25.
    let t = QualityThresholds::default();
    let img = checker(16, 16);
    let at_min = face(0.25, 0.2, 0.5, 0.8); // width exactly 0.25
    let below_min = face(0.25, 0.2, 0.499, 0.8); // width 0.249
    let at_max = face(0.0, 0.2, t.max_face_width_fraction, 0.8); // width equals the max exactly
    let above_max = face(0.0, 0.2, 0.95, 0.8);
    assert_eq!(t.min_face_width_fraction, 0.25);
    assert_eq!(measure(&img, Some(&at_min), &t).face_size, Verdict::Pass);
    assert_eq!(measure(&img, Some(&below_min), &t).face_size, Verdict::Fail);
    assert_eq!(measure(&img, Some(&at_max), &t).face_size, Verdict::Pass);
    assert_eq!(measure(&img, Some(&above_max), &t).face_size, Verdict::Fail);
}

#[test]
fn face_touching_frame_edge_fails_framing() {
    let t = QualityThresholds::default();
    let img = checker(16, 16);
    let cut_off = face(0.0, 0.2, 0.4, 0.8);
    let on_margin = face(t.frame_margin, 0.2, 0.4 + t.frame_margin, 0.8);
    assert_eq!(measure(&img, Some(&cut_off), &t).framing, Verdict::Fail);
    assert_eq!(measure(&img, Some(&on_margin), &t).framing, Verdict::Pass);
}

// ---- detection classification ----

fn det(score: f32) -> FaceDetection {
    FaceDetection {
        score,
        ..centred_face()
    }
}

#[test]
fn zero_one_and_many_faces_are_distinct_outcomes() {
    assert_eq!(classify(vec![], 0.5), DetectionOutcome::NoFace);
    assert!(matches!(
        classify(vec![det(0.9)], 0.5),
        DetectionOutcome::OneFace(_)
    ));
    assert_eq!(
        classify(vec![det(0.9), det(0.8)], 0.5),
        DetectionOutcome::MultipleFaces { count: 2 }
    );
}

#[test]
fn low_confidence_detections_are_dropped_before_counting() {
    assert_eq!(classify(vec![det(0.4)], 0.5), DetectionOutcome::NoFace);
    assert!(matches!(
        classify(vec![det(0.9), det(0.1)], 0.5),
        DetectionOutcome::OneFace(_)
    ));
}

#[test]
fn score_threshold_is_inclusive_and_nan_scores_are_dropped() {
    assert!(matches!(
        classify(vec![det(0.5)], 0.5),
        DetectionOutcome::OneFace(_)
    ));
    assert_eq!(classify(vec![det(f32::NAN)], 0.5), DetectionOutcome::NoFace);
}

// ---- box validation ----

#[test]
fn invalid_boxes_are_rejected_with_controlled_error() {
    for (x0, y0, x1, y1) in [
        (f32::NAN, 0.0, 0.5, 0.5),
        (-0.1, 0.0, 0.5, 0.5),
        (0.0, 0.0, 1.1, 0.5),
        (0.6, 0.0, 0.4, 0.5),
        (0.2, 0.3, 0.2, 0.9),
    ] {
        assert_eq!(
            NormalizedBox::new(x0, y0, x1, y1),
            Err(InferenceError::UnexpectedShape),
            "{x0} {y0} {x1} {y1}"
        );
    }
}

// ---- detector trait and errors ----

struct FixedDetector(Result<Vec<FaceDetection>, InferenceError>);

impl FaceDetector for FixedDetector {
    fn detect(&self, _image: &NormalizedImage) -> Result<Vec<FaceDetection>, InferenceError> {
        self.0.clone()
    }
}

#[test]
fn detector_trait_passes_detections_through_classification() {
    let detector = FixedDetector(Ok(vec![det(0.95)]));
    let outcome = classify(detector.detect(&checker(16, 16)).unwrap(), 0.5);
    assert!(matches!(outcome, DetectionOutcome::OneFace(_)));
}

#[test]
fn detector_errors_are_controlled_and_fixed_messages() {
    let detector = FixedDetector(Err(InferenceError::ArtifactMissing));
    let err = detector.detect(&checker(4, 4)).unwrap_err();
    assert_eq!(err, InferenceError::ArtifactMissing);
    for e in [
        InferenceError::ArtifactMissing,
        InferenceError::ArtifactInvalid,
        InferenceError::UnexpectedShape,
        InferenceError::Runtime,
    ] {
        let message = e.to_string();
        assert!(
            !message.contains('/') && !message.contains('\\'),
            "{message}"
        );
    }
}
