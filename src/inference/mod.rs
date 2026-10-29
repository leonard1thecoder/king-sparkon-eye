//! Face detection interface and detection outcomes (issue #24).
//!
//! The ONNX-backed detector is not implemented yet. Its model artifact, licence and
//! runtime still need a decision (see README). Everything here is model-independent:
//! the `FaceDetector` trait, validation of detector output, and the zero/one/many
//! classification. HTTP handlers depend only on this interface.
//!
//! Detection is not identity comparison and not liveness detection.

pub mod sface;
pub mod yunet;

pub use sface::*;

use crate::ingress::NormalizedImage;

/// Axis-aligned box in fractions of the image size, with origin at the top-left.
/// Invariants: all values are finite and in `[0, 1]`, and `x0 < x1` and `y0 < y1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizedBox {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl NormalizedBox {
    pub fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Result<Self, InferenceError> {
        let all_finite = [x0, y0, x1, y1].iter().all(|v| v.is_finite());
        let in_range = [x0, y0, x1, y1].iter().all(|v| (0.0..=1.0).contains(v));
        if !all_finite || !in_range || x0 >= x1 || y0 >= y1 {
            return Err(InferenceError::UnexpectedShape);
        }
        Ok(Self { x0, y0, x1, y1 })
    }

    pub fn width(&self) -> f32 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f32 {
        self.y1 - self.y0
    }
}

/// One detected face.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceDetection {
    pub bbox: NormalizedBox,
    /// Detector confidence in `[0, 1]`.
    pub score: f32,
    /// Landmark points in the same normalised coordinate space as `bbox`.
    pub landmarks: Vec<[f32; 2]>,
}

/// Zero, one, or several faces. These are distinct outcomes and are never collapsed.
#[derive(Debug, Clone, PartialEq)]
pub enum DetectionOutcome {
    NoFace,
    OneFace(FaceDetection),
    MultipleFaces { count: usize },
}

/// Controlled inference errors. Messages are fixed, and paths and runtime details are never exposed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum InferenceError {
    #[error("detector model artifact is missing")]
    ArtifactMissing,
    #[error("detector model artifact is invalid")]
    ArtifactInvalid,
    #[error("detector output has an unexpected shape or value")]
    UnexpectedShape,
    #[error("detector runtime failed")]
    Runtime,
}

/// Any face detector. Implementations must return only validated [`FaceDetection`]s.
pub trait FaceDetector {
    fn detect(&self, image: &NormalizedImage) -> Result<Vec<FaceDetection>, InferenceError>;
}

/// Keeps detections at or above `min_score` and classifies the result as zero, one, or several faces.
pub fn classify(detections: Vec<FaceDetection>, min_score: f32) -> DetectionOutcome {
    let mut kept: Vec<FaceDetection> = detections
        .into_iter()
        .filter(|d| d.score.is_finite() && d.score >= min_score)
        .collect();
    match kept.len() {
        0 => DetectionOutcome::NoFace,
        1 => DetectionOutcome::OneFace(kept.remove(0)),
        count => DetectionOutcome::MultipleFaces { count },
    }
}
