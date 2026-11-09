//! SFace adapter tests. These tests use synthetic pixel buffers only; they do not
//! contain biometric photographs and do not establish recognition accuracy.

use std::path::{Path, PathBuf};

use king_sparkon_eye::inference::sface::{
    EMBEDDING_DIM, FaceEmbedding, FaceRecognizer, MockFaceRecognizer, SFACE_INPUT_SIZE,
    SFaceRecognizer, align_face, preprocess_aligned_face,
};
use king_sparkon_eye::inference::{FaceDetection, InferenceError, NormalizedBox};
use king_sparkon_eye::ingress::{NormalizedImage, SupportedFormat};

fn image(width: u32, height: u32, pixel: [u8; 3]) -> NormalizedImage {
    let mut rgb = Vec::with_capacity((width * height * 3) as usize);
    for _ in 0..(width * height) {
        rgb.extend_from_slice(&pixel);
    }
    NormalizedImage {
        format: SupportedFormat::Png,
        width,
        height,
        rgb,
    }
}

fn detection_for_canonical_crop() -> FaceDetection {
    let points = [
        [38.2946f32, 51.6963f32],
        [73.5318, 51.5014],
        [56.0252, 71.7366],
        [41.5493, 92.3655],
        [70.7299, 92.2041],
    ];
    FaceDetection {
        bbox: NormalizedBox::new(0.1, 0.1, 0.9, 0.9).unwrap(),
        score: 1.0,
        landmarks: points
            .iter()
            .map(|point| [point[0] / 112.0, point[1] / 112.0])
            .collect(),
    }
}

fn runtime_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("artifacts/onnxruntime/1.31.1/linux-x86_64/libonnxruntime.so.1.31.1")
}

#[test]
fn canonical_landmarks_produce_a_112_square_aligned_crop() {
    let source = image(112, 112, [40, 80, 120]);
    let aligned = align_face(&source, &detection_for_canonical_crop()).unwrap();
    assert_eq!(aligned.width, SFACE_INPUT_SIZE as u32);
    assert_eq!(aligned.height, SFACE_INPUT_SIZE as u32);
    let center = ((56 * 112 + 56) * 3) as usize;
    assert_eq!(&aligned.rgb[center..center + 3], &[40, 80, 120]);
}

#[test]
fn alignment_rejects_missing_or_out_of_range_landmarks() {
    let source = image(112, 112, [0, 0, 0]);
    let mut detection = detection_for_canonical_crop();
    detection.landmarks.pop();
    assert_eq!(
        align_face(&source, &detection).err(),
        Some(InferenceError::UnexpectedShape)
    );

    let mut detection = detection_for_canonical_crop();
    detection.landmarks[0][0] = f32::NAN;
    assert_eq!(
        align_face(&source, &detection).err(),
        Some(InferenceError::UnexpectedShape)
    );
}

#[test]
fn aligned_input_is_rgb_nchw_with_unscaled_byte_values() {
    let aligned = image(112, 112, [10, 20, 30]);
    let tensor = preprocess_aligned_face(&aligned).unwrap();
    let plane = 112 * 112;
    assert_eq!(tensor.len(), 3 * plane);
    assert_eq!(tensor[0], 10.0);
    assert_eq!(tensor[plane], 20.0);
    assert_eq!(tensor[2 * plane], 30.0);
}

#[test]
fn aligned_input_rejects_wrong_dimensions_and_malformed_buffers() {
    assert_eq!(
        preprocess_aligned_face(&image(111, 112, [0, 0, 0])).err(),
        Some(InferenceError::UnexpectedShape)
    );
    let malformed = NormalizedImage {
        format: SupportedFormat::Png,
        width: 112,
        height: 112,
        rgb: vec![0; 9],
    };
    assert_eq!(
        preprocess_aligned_face(&malformed).err(),
        Some(InferenceError::UnexpectedShape)
    );
}

#[test]
fn embeddings_validate_dimension_finiteness_and_normalize() {
    let raw = vec![1.0f32; EMBEDDING_DIM];
    let embedding = FaceEmbedding::new(&raw).unwrap();
    let norm = embedding
        .as_slice()
        .iter()
        .map(|value| value * value)
        .sum::<f32>()
        .sqrt();
    assert!((norm - 1.0).abs() < 1e-6);

    assert_eq!(
        FaceEmbedding::new(&[1.0, 2.0]).err(),
        Some(InferenceError::UnexpectedShape)
    );
    let mut invalid = vec![1.0f32; EMBEDDING_DIM];
    invalid[17] = f32::INFINITY;
    assert_eq!(
        FaceEmbedding::new(&invalid).err(),
        Some(InferenceError::UnexpectedShape)
    );
    assert_eq!(
        FaceEmbedding::new(&vec![0.0f32; EMBEDDING_DIM]).err(),
        Some(InferenceError::UnexpectedShape)
    );
}

#[test]
fn mock_recognizer_remains_available_without_model_weights() {
    let recognizer = MockFaceRecognizer::new();
    let embedding = recognizer
        .extract_embedding(&image(112, 112, [0, 0, 0]))
        .unwrap();
    assert_eq!(embedding.as_slice().len(), EMBEDDING_DIM);
}


#[test]
fn corrupt_model_artifact_fails_digest_verification() {
    let dir = std::env::temp_dir().join(format!("eye-sface-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bad_model = dir.join("wrong-model.onnx");
    std::fs::write(&bad_model, b"not the pinned SFace model").unwrap();
    assert_eq!(
        SFaceRecognizer::load(&bad_model, &runtime_path()).err(),
        Some(InferenceError::ArtifactInvalid)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_model_artifact_fails_closed_before_runtime_initialization() {
    let missing = Path::new(env!("CARGO_MANIFEST_DIR")).join("missing-sface.onnx");
    assert_eq!(
        SFaceRecognizer::load(&missing, &runtime_path()).err(),
        Some(InferenceError::ArtifactMissing)
    );
}
