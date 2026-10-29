//! ONNX Face-Recognition embedding adapter (issue #59).
//!
//! Extracts a 128-dimensional L2-normalized feature vector from an aligned face image
//! according to the contract defined in `docs/ONNX_MODEL_CONTRACT_INVENTORY.md` (issue #58).
//!
//! Non-negotiable rules enforced here:
//! - Embeddings must strictly have dimension 128 (`EMBEDDING_DIM`).
//! - All elements must be finite: no `NaN`, `+Infinity`, or `-Infinity`.
//! - Vectors must be L2-normalized ($||\mathbf{v}||_2 = 1.0$).
//! - Cosine similarity calculation is provided, operating in the range `[-1.0, 1.0]`.
//! - Comparison results represent candidate signals only, never admission authorizations.
//! - Raw embeddings are protected biometric data: they must not be logged or exposed publicly.

use crate::inference::InferenceError;
use crate::ingress::NormalizedImage;

/// Standard embedding vector dimension required by the contract.
pub const EMBEDDING_DIM: usize = 128;

/// Model identifier for the 128-D recognizer.
pub const SFACE_MODEL_ID: &str = "sface-2021dec";

/// Normalized 128-dimensional biometric face embedding vector.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceEmbedding {
    vector: [f32; EMBEDDING_DIM],
}

impl FaceEmbedding {
    /// Creates a new `FaceEmbedding` from a raw slice, verifying dimension, finite values, and L2 normalization.
    pub fn new(raw: &[f32]) -> Result<Self, InferenceError> {
        if raw.len() != EMBEDDING_DIM {
            return Err(InferenceError::UnexpectedShape);
        }

        if !raw.iter().all(|x| x.is_finite()) {
            return Err(InferenceError::UnexpectedShape);
        }

        let norm_sq: f32 = raw.iter().map(|x| x * x).sum();
        let norm = norm_sq.sqrt();
        if norm <= f32::EPSILON || !norm.is_finite() {
            return Err(InferenceError::UnexpectedShape);
        }

        let mut vector = [0.0f32; EMBEDDING_DIM];
        for (i, val) in raw.iter().enumerate() {
            vector[i] = val / norm;
        }

        Ok(Self { vector })
    }

    /// Access the underlying normalized 128-D slice.
    pub fn as_slice(&self) -> &[f32] {
        &self.vector
    }

    /// Calculates cosine similarity with another embedding. Range is `[-1.0, 1.0]`.
    pub fn cosine_similarity(&self, other: &Self) -> f32 {
        let dot: f32 = self
            .vector
            .iter()
            .zip(other.vector.iter())
            .map(|(a, b)| a * b)
            .sum();
        dot.clamp(-1.0, 1.0)
    }
}

/// Face recognition embedding extractor trait.
pub trait FaceRecognizer: Send + Sync {
    /// Extracts a 128-D embedding from an aligned face crop.
    fn extract_embedding(&self, aligned_face: &NormalizedImage) -> Result<FaceEmbedding, InferenceError>;
}

/// Deterministic mock recognizer for testing and verification without weights.
#[derive(Debug, Default)]
pub struct MockFaceRecognizer {
    mock_vector: Option<[f32; EMBEDDING_DIM]>,
    fail_with: Option<InferenceError>,
}

impl MockFaceRecognizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_mock_vector(mut self, vector: [f32; EMBEDDING_DIM]) -> Self {
        self.mock_vector = Some(vector);
        self
    }

    pub fn with_failure(mut self, err: InferenceError) -> Self {
        self.fail_with = Some(err);
        self
    }
}

impl FaceRecognizer for MockFaceRecognizer {
    fn extract_embedding(&self, _aligned_face: &NormalizedImage) -> Result<FaceEmbedding, InferenceError> {
        if let Some(err) = self.fail_with {
            return Err(err);
        }
        let raw = self.mock_vector.unwrap_or([0.1f32; EMBEDDING_DIM]);
        FaceEmbedding::new(&raw)
    }
}
