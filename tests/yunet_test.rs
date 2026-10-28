//! YuNet adapter tests. Decoding and preprocessing use hand-built tensors, so their correct
//! answers can be worked out in advance. Loading and running use the vendored artifacts.

use std::path::{Path, PathBuf};

use king_sparkon_eye::inference::yunet::{
    DEFAULT_NMS_THRESHOLD, DEFAULT_SCORE_THRESHOLD, Letterbox, StrideOutputs, YunetDetector,
    anchors_for_stride, decode, preprocess,
};
use king_sparkon_eye::inference::{FaceDetector, InferenceError};
use king_sparkon_eye::ingress::{NormalizedImage, SupportedFormat};

fn root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn model_path() -> PathBuf {
    root().join("artifacts/models/yunet-2023mar/face_detection_yunet_2023mar.onnx")
}

fn runtime_path() -> PathBuf {
    root().join("artifacts/onnxruntime/1.31.1/linux-x86_64/libonnxruntime.so.1.31.1")
}

fn image(width: u32, height: u32, rgb: impl Fn(u32, u32) -> [u8; 3]) -> NormalizedImage {
    let mut data = Vec::with_capacity((width * height * 3) as usize);
    for y in 0..height {
        for x in 0..width {
            data.extend_from_slice(&rgb(x, y));
        }
    }
    NormalizedImage {
        format: SupportedFormat::Png,
        width,
        height,
        rgb: data,
    }
}

/// All-zero tensors: every anchor scores zero, so no face should be found.
struct Blank {
    cls: Vec<Vec<f32>>,
    obj: Vec<Vec<f32>>,
    bbox: Vec<Vec<f32>>,
    kps: Vec<Vec<f32>>,
}

impl Blank {
    fn new() -> Self {
        let n = |s| anchors_for_stride(s);
        Self {
            cls: STRIDES.iter().map(|s| vec![0.0; n(*s)]).collect(),
            obj: STRIDES.iter().map(|s| vec![0.0; n(*s)]).collect(),
            bbox: STRIDES.iter().map(|s| vec![0.0; n(*s) * 4]).collect(),
            kps: STRIDES.iter().map(|s| vec![0.0; n(*s) * 10]).collect(),
        }
    }

    fn outputs(&self) -> StrideOutputs<'_> {
        StrideOutputs {
            cls: [&self.cls[0], &self.cls[1], &self.cls[2]],
            obj: [&self.obj[0], &self.obj[1], &self.obj[2]],
            bbox: [&self.bbox[0], &self.bbox[1], &self.bbox[2]],
            kps: [&self.kps[0], &self.kps[1], &self.kps[2]],
        }
    }
}

const STRIDES: [usize; 3] = [8, 16, 32];
const FULL: Letterbox = Letterbox {
    content_w: 640.0,
    content_h: 640.0,
};

// ---- artifact loading errors ----

#[test]
fn missing_model_returns_artifact_missing() {
    let err = YunetDetector::load(&root().join("does/not/exist.onnx"), &runtime_path())
        .err()
        .unwrap();
    assert_eq!(err, InferenceError::ArtifactMissing);
}

#[test]
fn missing_runtime_returns_artifact_missing() {
    let err = YunetDetector::load(&model_path(), &root().join("does/not/libonnxruntime.so"))
        .err()
        .unwrap();
    assert_eq!(err, InferenceError::ArtifactMissing);
}

#[test]
fn corrupt_model_returns_artifact_invalid() {
    let dir = std::env::temp_dir().join(format!("eye-yunet-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bad = dir.join("corrupt.onnx");
    std::fs::write(&bad, b"this is not an onnx model").unwrap();
    let err = YunetDetector::load(&bad, &runtime_path()).err().unwrap();
    assert_eq!(err, InferenceError::ArtifactInvalid);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn vendored_model_loads_and_passes_io_validation() {
    YunetDetector::load(&model_path(), &runtime_path()).expect("vendored artifacts load");
}

#[test]
fn blank_image_yields_no_face_on_the_real_model() {
    let detector = YunetDetector::load(&model_path(), &runtime_path()).unwrap();
    let blank = image(320, 240, |_, _| [128, 128, 128]);
    let faces = detector.detect(&blank).expect("inference runs");
    assert!(
        faces.is_empty(),
        "a flat grey image should not produce faces: {faces:?}"
    );
}

#[test]
fn real_model_output_is_finite_and_in_bounds_on_noise() {
    let detector = YunetDetector::load(&model_path(), &runtime_path()).unwrap();
    // Deterministic pseudo-noise, so the test is repeatable.
    let noise = image(200, 150, |x, y| {
        let v = (x
            .wrapping_mul(2654435761)
            .wrapping_add(y.wrapping_mul(40503))) as u8;
        [v, v.wrapping_add(50), v.wrapping_add(100)]
    });
    for face in detector.detect(&noise).expect("inference runs") {
        assert!(face.score.is_finite() && (0.0..=1.0).contains(&face.score));
        assert!(face.score >= DEFAULT_SCORE_THRESHOLD);
        for v in [face.bbox.x0, face.bbox.y0, face.bbox.x1, face.bbox.y1] {
            assert!((0.0..=1.0).contains(&v));
        }
    }
}

// ---- preprocessing ----

#[test]
fn preprocess_writes_bgr_in_the_letterboxed_region_and_zeros_the_padding() {
    // 2:1 landscape, pure red (R=255, G=0, B=0). Content is 640x320, padding is the lower half.
    let red = image(40, 20, |_, _| [255, 0, 0]);
    let (tensor, geometry) = preprocess(&red).unwrap();
    assert_eq!(
        geometry,
        Letterbox {
            content_w: 640.0,
            content_h: 320.0
        }
    );
    let plane = 640 * 640;
    let inside = 10 * 640 + 10;
    assert_eq!(tensor[inside], 0.0, "blue plane is zero for red pixels");
    assert_eq!(
        tensor[plane + inside],
        0.0,
        "green plane is zero for red pixels"
    );
    assert_eq!(
        tensor[2 * plane + inside],
        255.0,
        "red sits in the third (BGR) plane"
    );
    let padding = 500 * 640 + 10;
    assert_eq!(tensor[2 * plane + padding], 0.0, "padding is zero");
}

#[test]
fn preprocess_rejects_inconsistent_buffers() {
    let bad = NormalizedImage {
        format: SupportedFormat::Png,
        width: 4,
        height: 4,
        rgb: vec![0; 10],
    };
    assert_eq!(
        preprocess(&bad).err(),
        Some(InferenceError::UnexpectedShape)
    );
}

// ---- decoding (hand-built tensors) ----

#[test]
fn a_single_strong_anchor_decodes_to_the_expected_box() {
    // Stride 8, cell row 10, column 20. Zero offsets and log-scales give a box of exactly one stride.
    let mut t = Blank::new();
    let idx = 10 * 80 + 20;
    t.cls[0][idx] = 1.0;
    t.obj[0][idx] = 0.25; // score = sqrt(1 * 0.25) = 0.5
    let faces = decode(&t.outputs(), FULL, 0.5, DEFAULT_NMS_THRESHOLD).unwrap();
    assert_eq!(faces.len(), 1);
    let b = faces[0].bbox;
    // Centre = (20*8, 10*8) = (160, 80); size 8x8. Corners are (156, 76) and (164, 84).
    assert!((b.x0 - 156.0 / 640.0).abs() < 1e-6);
    assert!((b.y0 - 76.0 / 640.0).abs() < 1e-6);
    assert!((b.x1 - 164.0 / 640.0).abs() < 1e-6);
    assert!((b.y1 - 84.0 / 640.0).abs() < 1e-6);
    assert!((faces[0].score - 0.5).abs() < 1e-6);
}

#[test]
fn score_threshold_is_inclusive() {
    let mut t = Blank::new();
    let idx = 0;
    t.cls[0][idx] = 1.0;
    t.obj[0][idx] = 0.25; // score exactly 0.5
    assert_eq!(decode(&t.outputs(), FULL, 0.5, 0.3).unwrap().len(), 1);
    assert_eq!(decode(&t.outputs(), FULL, 0.5001, 0.3).unwrap().len(), 0);
}

#[test]
fn classification_and_objectness_are_clamped_before_scoring() {
    let mut t = Blank::new();
    t.cls[0][0] = 4.0; // clamps to 1
    t.obj[0][0] = 1.0; // score 1.0
    let faces = decode(&t.outputs(), FULL, 0.5, 0.3).unwrap();
    assert!((faces[0].score - 1.0).abs() < 1e-6);
}

#[test]
fn overlapping_boxes_are_suppressed_by_score() {
    let mut t = Blank::new();
    // Two adjacent stride-8 anchors produce heavily overlapping boxes.
    let a = 10 * 80 + 20;
    let b = 10 * 80 + 21;
    // Log-scale ln(4) makes each box 32 px wide, so the two boxes (centres 8 px apart) overlap at IoU 0.6.
    let log4 = 4.0f32.ln();
    t.bbox[0][a * 4 + 2] = log4;
    t.bbox[0][a * 4 + 3] = log4;
    t.bbox[0][b * 4 + 2] = log4;
    t.bbox[0][b * 4 + 3] = log4;
    t.cls[0][a] = 1.0;
    t.obj[0][a] = 0.81; // score 0.9
    t.cls[0][b] = 1.0;
    t.obj[0][b] = 0.49; // score 0.7
    let faces = decode(&t.outputs(), FULL, 0.5, 0.3).unwrap();
    assert_eq!(faces.len(), 1, "IoU of the two boxes is well above 0.3");
    assert!(
        (faces[0].score - 0.9).abs() < 1e-5,
        "the higher-scoring box survives"
    );
}

#[test]
fn distant_boxes_are_both_kept() {
    let mut t = Blank::new();
    for idx in [10 * 80 + 20, 60 * 80 + 60] {
        t.cls[0][idx] = 1.0;
        t.obj[0][idx] = 0.25;
    }
    assert_eq!(decode(&t.outputs(), FULL, 0.5, 0.3).unwrap().len(), 2);
}

#[test]
fn boxes_outside_the_content_area_are_dropped_or_clipped() {
    // Letterboxed content covers only the top-left 320x320 of the model square.
    let geometry = Letterbox {
        content_w: 320.0,
        content_h: 320.0,
    };
    let mut t = Blank::new();
    let idx = 5 * 80 + 60; // centre (480, 40) in model pixels: outside the content width
    t.cls[0][idx] = 1.0;
    t.obj[0][idx] = 0.25;
    let faces = decode(&t.outputs(), geometry, 0.5, 0.3).unwrap();
    // The box lies entirely past the content edge. Clipping leaves zero area, so it is dropped.
    assert!(faces.is_empty());
}

#[test]
fn landmarks_are_decoded_and_normalised() {
    let mut t = Blank::new();
    let idx = 0;
    t.cls[0][idx] = 1.0;
    t.obj[0][idx] = 0.25;
    // First landmark offset (0.5, 0.5) at cell (0,0) of stride 8 gives (4, 4) in model pixels.
    t.kps[0][0] = 0.5;
    t.kps[0][1] = 0.5;
    let faces = decode(&t.outputs(), FULL, 0.5, 0.3).unwrap();
    let lm = faces[0].landmarks[0];
    assert!((lm[0] - 4.0 / 640.0).abs() < 1e-6);
    assert!((lm[1] - 4.0 / 640.0).abs() < 1e-6);
    assert_eq!(faces[0].landmarks.len(), 5);
}

#[test]
fn wrong_output_lengths_are_rejected() {
    let mut t = Blank::new();
    t.cls[0].pop();
    assert_eq!(
        decode(&t.outputs(), FULL, 0.5, 0.3).err(),
        Some(InferenceError::UnexpectedShape)
    );
}

#[test]
fn zero_one_and_many_are_separate_outcomes_on_synthetic_tensors() {
    use king_sparkon_eye::inference::{DetectionOutcome, classify};
    let none = Blank::new();
    let faces = decode(&none.outputs(), FULL, 0.5, 0.3).unwrap();
    assert_eq!(classify(faces, 0.5), DetectionOutcome::NoFace);

    let mut one = Blank::new();
    one.cls[0][0] = 1.0;
    one.obj[0][0] = 0.25;
    let faces = decode(&one.outputs(), FULL, 0.5, 0.3).unwrap();
    assert!(matches!(classify(faces, 0.5), DetectionOutcome::OneFace(_)));

    let mut many = Blank::new();
    for idx in [10 * 80 + 20, 60 * 80 + 60] {
        many.cls[0][idx] = 1.0;
        many.obj[0][idx] = 0.25;
    }
    let faces = decode(&many.outputs(), FULL, 0.5, 0.3).unwrap();
    assert_eq!(
        classify(faces, 0.5),
        DetectionOutcome::MultipleFaces { count: 2 }
    );
}
