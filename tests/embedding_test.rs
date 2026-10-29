//! Tests for the face recognition embedding adapter (issue #59).

use king_sparkon_eye::inference::{
    FaceEmbedding, FaceRecognizer, InferenceError, MockFaceRecognizer, EMBEDDING_DIM,
};
use king_sparkon_eye::ingress::NormalizedImage;

fn sample_image() -> NormalizedImage {
    NormalizedImage {
        width: 112,
        height: 112,
        rgba: vec![128; 112 * 112 * 4],
    }
}

#[test]
fn embedding_dimension_must_be_strictly_128() {
    let too_short = vec![0.5f32; 64];
    assert_eq!(
        FaceEmbedding::new(&too_short),
        Err(InferenceError::UnexpectedShape)
    );

    let too_long = vec![0.5f32; 256];
    assert_eq!(
        FaceEmbedding::new(&too_long),
        Err(InferenceError::UnexpectedShape)
    );
}

#[test]
fn embedding_rejects_nan_and_infinities() {
    let mut with_nan = vec![0.1f32; EMBEDDING_DIM];
    with_nan[10] = f32::NAN;
    assert_eq!(
        FaceEmbedding::new(&with_nan),
        Err(InferenceError::UnexpectedShape)
    );

    let mut with_inf = vec![0.1f32; EMBEDDING_DIM];
    with_inf[20] = f32::INFINITY;
    assert_eq!(
        FaceEmbedding::new(&with_inf),
        Err(InferenceError::UnexpectedShape)
    );
}

#[test]
fn embedding_is_l2_normalized() {
    let raw = vec![2.0f32; EMBEDDING_DIM];
    let emb = FaceEmbedding::new(&raw).expect("valid embedding");

    let norm_sq: f32 = emb.as_slice().iter().map(|x| x * x).sum();
    let norm = norm_sq.sqrt();
    assert!((norm - 1.0).abs() < 1e-5, "Norm must be 1.0");
}

#[test]
fn cosine_similarity_identical_and_orthogonal() {
    let raw1 = vec![1.0f32; EMBEDDING_DIM];
    let emb1 = FaceEmbedding::new(&raw1).unwrap();
    let emb2 = FaceEmbedding::new(&raw1).unwrap();

    let sim_identical = emb1.cosine_similarity(&emb2);
    assert!((sim_identical - 1.0).abs() < 1e-5, "Identical vectors must have cosine similarity 1.0");

    let mut raw_opposite = vec![-1.0f32; EMBEDDING_DIM];
    let emb_opposite = FaceEmbedding::new(&raw_opposite).unwrap();
    let sim_opposite = emb1.cosine_similarity(&emb_opposite);
    assert!((sim_opposite - (-1.0)).abs() < 1e-5, "Opposite vectors must have cosine similarity -1.0");
}

#[test]
fn mock_face_recognizer_extracts_embeddings() {
    let recognizer = MockFaceRecognizer::new();
    let image = sample_image();
    let result = recognizer.extract_embedding(&image);
    assert!(result.is_ok());
    let emb = result.unwrap();
    assert_eq!(emb.as_slice().len(), EMBEDDING_DIM);
}

#[test]
fn mock_face_recognizer_propagates_controlled_errors() {
    let recognizer = MockFaceRecognizer::new().with_failure(InferenceError::Runtime);
    let image = sample_image();
    let result = recognizer.extract_embedding(&image);
    assert_eq!(result, Err(InferenceError::Runtime));
}
