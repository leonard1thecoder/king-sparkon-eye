//! SFace 2021dec face embedding adapter (issue #59).
//!
//! The adapter implements OpenCV's published FaceRecognizerSF alignment and feature-input
//! contract using the shared ONNX Runtime environment. Model weights are supplied externally;
//! this repository does not download or vendor the SFace checkpoint while provenance review is
//! unresolved. Constructing this adapter does not register it with an API or worker runtime.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::sync::Mutex;

use ort::session::Session;
use ort::value::Tensor;
use sha2::{Digest, Sha256};

use crate::ingress::NormalizedImage;

use super::runtime::environment;
use super::{FaceDetection, InferenceError};

/// Standard embedding vector dimension required by the SFace contract.
pub const EMBEDDING_DIM: usize = 128;
/// SFace model identifier.
pub const SFACE_MODEL_ID: &str = "sface-2021dec";
/// SFace aligned input edge length.
pub const SFACE_INPUT_SIZE: usize = 112;
/// SHA-256 of the exact OpenCV Zoo FP32 2021dec checkpoint.
pub const SFACE_MODEL_SHA256: &str =
    "0ba9fbfa01b5270c96627c4ef784da859931e02f04419c829e83484087c34e79";
const SFACE_MODEL_SIZE_BYTES: u64 = 38_696_353;

/// Standard SFace five-landmark destination coordinates in 112×112 pixel space.
/// Order: subject's right eye, subject's left eye, nose, subject's right mouth corner,
/// subject's left mouth corner. This matches YuNet/OpenCV's documented landmark order.
const TARGET_LANDMARKS: [[f64; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// Normalized 128-dimensional biometric face embedding vector.
#[derive(Debug, Clone, PartialEq)]
pub struct FaceEmbedding {
    vector: [f32; EMBEDDING_DIM],
}

impl FaceEmbedding {
    /// Creates a new embedding, validating dimension and finiteness and applying L2 normalization.
    pub fn new(raw: &[f32]) -> Result<Self, InferenceError> {
        if raw.len() != EMBEDDING_DIM || !raw.iter().all(|value| value.is_finite()) {
            return Err(InferenceError::UnexpectedShape);
        }

        let norm_sq: f32 = raw.iter().map(|value| value * value).sum();
        let norm = norm_sq.sqrt();
        if norm <= f32::EPSILON || !norm.is_finite() {
            return Err(InferenceError::UnexpectedShape);
        }

        let mut vector = [0.0f32; EMBEDDING_DIM];
        for (index, value) in raw.iter().enumerate() {
            vector[index] = value / norm;
        }
        Ok(Self { vector })
    }

    /// Access the normalized 128-D vector. Callers must treat it as protected biometric data.
    pub fn as_slice(&self) -> &[f32] {
        &self.vector
    }

    /// Calculates cosine similarity. This is a candidate signal, not an identity decision.
    pub fn cosine_similarity(&self, other: &Self) -> f32 {
        let dot: f32 = self
            .vector
            .iter()
            .zip(other.vector.iter())
            .map(|(left, right)| left * right)
            .sum();
        dot.clamp(-1.0, 1.0)
    }
}

/// Extracts a normalized embedding from an already aligned 112×112 RGB crop.
pub trait FaceRecognizer: Send + Sync {
    fn extract_embedding(
        &self,
        aligned_face: &NormalizedImage,
    ) -> Result<FaceEmbedding, InferenceError>;
}

/// Deterministic mock recognizer for tests that do not load biometric model weights.
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

    pub fn with_failure(mut self, error: InferenceError) -> Self {
        self.fail_with = Some(error);
        self
    }
}

impl FaceRecognizer for MockFaceRecognizer {
    fn extract_embedding(
        &self,
        _aligned_face: &NormalizedImage,
    ) -> Result<FaceEmbedding, InferenceError> {
        if let Some(error) = self.fail_with {
            return Err(error);
        }
        FaceEmbedding::new(&self.mock_vector.unwrap_or([0.1f32; EMBEDDING_DIM]))
    }
}

/// Production SFace recognizer. It is deliberately not wired into the HTTP or worker runtime.
///
/// The input/output names are read from the ONNX graph instead of guessed. The model contract
/// requires one FP32 input shaped [1, 3, 112, 112] and one FP32 output shaped [1, 128].
pub struct SFaceRecognizer {
    session: Mutex<Session>,
    input_name: String,
    output_name: String,
}

impl SFaceRecognizer {
    /// Loads a caller-supplied model artifact using the process-wide ONNX Runtime environment.
    ///
    /// Missing files, incompatible model metadata and runtime failures return fixed errors without
    /// exposing local paths or native runtime details.
    pub fn load(model_path: &Path, runtime_path: &Path) -> Result<Self, InferenceError> {
        if !model_path.is_file() || !runtime_path.is_file() {
            return Err(InferenceError::ArtifactMissing);
        }
        verify_model_artifact(model_path)?;

        let runtime = environment(runtime_path)?;
        let session = Session::builder(runtime)
            .map_err(|_| InferenceError::Runtime)?
            .commit_from_file(model_path)
            .map_err(|_| InferenceError::ArtifactInvalid)?;
        let (input_name, output_name) = validate_io(&session)?;

        Ok(Self {
            session: Mutex::new(session),
            input_name,
            output_name,
        })
    }

    /// Aligns a source image from YuNet's five normalized landmarks, then runs SFace inference.
    ///
    /// This method is an internal inference primitive. It must only be called by an explicitly
    /// approved processor after the platform evaluation gate (#95) is satisfied.
    pub fn extract_from_detection(
        &self,
        image: &NormalizedImage,
        detection: &FaceDetection,
    ) -> Result<FaceEmbedding, InferenceError> {
        let aligned = align_face(image, detection)?;
        self.extract_embedding(&aligned)
    }

    fn infer(&self, aligned_face: &NormalizedImage) -> Result<FaceEmbedding, InferenceError> {
        let input = preprocess_aligned_face(aligned_face)?;
        let tensor = Tensor::from_array(([1usize, 3, SFACE_INPUT_SIZE, SFACE_INPUT_SIZE], input))
            .map_err(|_| InferenceError::Runtime)?;

        let mut session = self.session.lock().map_err(|_| InferenceError::Runtime)?;
        let outputs = session
            .run(ort::inputs![self.input_name.as_str() => tensor])
            .map_err(|_| InferenceError::Runtime)?;
        let (_, values) = outputs[self.output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(|_| InferenceError::UnexpectedShape)?;
        FaceEmbedding::new(values)
    }
}

impl FaceRecognizer for SFaceRecognizer {
    fn extract_embedding(
        &self,
        aligned_face: &NormalizedImage,
    ) -> Result<FaceEmbedding, InferenceError> {
        self.infer(aligned_face)
    }
}

fn validate_io(session: &Session) -> Result<(String, String), InferenceError> {
    let inputs = session.inputs();
    let outputs = session.outputs();
    if inputs.len() != 1 || outputs.len() != 1 {
        return Err(InferenceError::ArtifactInvalid);
    }

    let input_shape = inputs[0]
        .dtype()
        .tensor_shape()
        .ok_or(InferenceError::ArtifactInvalid)?;
    if input_shape
        .iter()
        .copied()
        .ne([1i64, 3, SFACE_INPUT_SIZE as i64, SFACE_INPUT_SIZE as i64])
    {
        return Err(InferenceError::ArtifactInvalid);
    }

    let output_shape = outputs[0]
        .dtype()
        .tensor_shape()
        .ok_or(InferenceError::ArtifactInvalid)?;
    if output_shape
        .iter()
        .copied()
        .ne([1i64, EMBEDDING_DIM as i64])
    {
        return Err(InferenceError::ArtifactInvalid);
    }

    Ok((inputs[0].name().to_owned(), outputs[0].name().to_owned()))
}

/// Builds the FP32 NCHW feature tensor expected by OpenCV's SFace implementation.
///
/// OpenCV calls blobFromImage(aligned, 1, Size(112,112), Scalar(0), true, false):
/// channel order is RGB, values remain in [0,255], and no mean/scale normalization is applied.
/// The ingress image is already RGB, so no channel swap is needed here.
pub fn preprocess_aligned_face(aligned_face: &NormalizedImage) -> Result<Vec<f32>, InferenceError> {
    validate_image(aligned_face)?;
    if aligned_face.width != SFACE_INPUT_SIZE as u32
        || aligned_face.height != SFACE_INPUT_SIZE as u32
    {
        return Err(InferenceError::UnexpectedShape);
    }

    let plane = SFACE_INPUT_SIZE * SFACE_INPUT_SIZE;
    let mut tensor = vec![0.0f32; 3 * plane];
    for (pixel_index, rgb) in aligned_face.rgb.chunks_exact(3).enumerate() {
        tensor[pixel_index] = f32::from(rgb[0]);
        tensor[plane + pixel_index] = f32::from(rgb[1]);
        tensor[2 * plane + pixel_index] = f32::from(rgb[2]);
    }
    Ok(tensor)
}

/// Aligns a detected face to the SFace 112×112 canonical crop using YuNet's five landmarks.
///
/// The similarity transform and destination landmarks match OpenCV FaceRecognizerSF::alignCrop.
/// The source image is RGB, so the returned crop remains RGB.
pub fn align_face(
    image: &NormalizedImage,
    detection: &FaceDetection,
) -> Result<NormalizedImage, InferenceError> {
    validate_image(image)?;
    if !detection.score.is_finite()
        || !(0.0..=1.0).contains(&detection.score)
        || detection.landmarks.len() != TARGET_LANDMARKS.len()
        || detection
            .landmarks
            .iter()
            .flatten()
            .any(|value| !value.is_finite() || !(0.0..=1.0).contains(value))
    {
        return Err(InferenceError::UnexpectedShape);
    }

    let source_points = detection
        .landmarks
        .iter()
        .map(|point| {
            [
                f64::from(point[0]) * f64::from(image.width),
                f64::from(point[1]) * f64::from(image.height),
            ]
        })
        .collect::<Vec<_>>();
    let transform = similarity_transform(&source_points)?;

    let width = SFACE_INPUT_SIZE as u32;
    let height = SFACE_INPUT_SIZE as u32;
    let mut rgb = vec![0u8; (width * height * 3) as usize];

    let [a, b, tx, ty] = transform;
    let determinant = a * a + b * b;
    if !determinant.is_finite() || determinant <= f64::EPSILON {
        return Err(InferenceError::UnexpectedShape);
    }

    for y in 0..height {
        for x in 0..width {
            // OpenCV warpAffine uses the inverse mapping when WARP_INVERSE_MAP is not set.
            let dx = f64::from(x) - tx;
            let dy = f64::from(y) - ty;
            let source_x = (a * dx + b * dy) / determinant;
            let source_y = (-b * dx + a * dy) / determinant;
            let offset = ((y * width + x) * 3) as usize;
            for channel in 0..3 {
                rgb[offset + channel] =
                    bilinear_sample(image, source_x, source_y, channel as usize);
            }
        }
    }

    Ok(NormalizedImage {
        format: image.format,
        width,
        height,
        rgb,
    })
}

/// Returns the forward similarity transform [a, b, tx, ty], where
/// x' = a*x - b*y + tx and y' = b*x + a*y + ty.
fn similarity_transform(source: &[[f64; 2]]) -> Result<[f64; 4], InferenceError> {
    if source.len() != TARGET_LANDMARKS.len() {
        return Err(InferenceError::UnexpectedShape);
    }

    let mut source_mean = [0.0f64; 2];
    let mut target_mean = [0.0f64; 2];
    for index in 0..5 {
        source_mean[0] += source[index][0] / 5.0;
        source_mean[1] += source[index][1] / 5.0;
        target_mean[0] += TARGET_LANDMARKS[index][0] / 5.0;
        target_mean[1] += TARGET_LANDMARKS[index][1] / 5.0;
    }

    let mut denominator = 0.0f64;
    let mut real = 0.0f64;
    let mut imaginary = 0.0f64;
    for index in 0..5 {
        let sx = source[index][0] - source_mean[0];
        let sy = source[index][1] - source_mean[1];
        let tx = TARGET_LANDMARKS[index][0] - target_mean[0];
        let ty = TARGET_LANDMARKS[index][1] - target_mean[1];
        denominator += sx * sx + sy * sy;
        real += sx * tx + sy * ty;
        imaginary += sx * ty - sy * tx;
    }

    if !denominator.is_finite() || denominator <= f64::EPSILON {
        return Err(InferenceError::UnexpectedShape);
    }

    let a = real / denominator;
    let b = imaginary / denominator;
    let tx = target_mean[0] - a * source_mean[0] + b * source_mean[1];
    let ty = target_mean[1] - b * source_mean[0] - a * source_mean[1];
    if ![a, b, tx, ty].iter().all(|value| value.is_finite()) {
        return Err(InferenceError::UnexpectedShape);
    }
    Ok([a, b, tx, ty])
}

fn bilinear_sample(image: &NormalizedImage, x: f64, y: f64, channel: usize) -> u8 {
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let x1 = x0 + 1;
    let y1 = y0 + 1;
    let dx = x - x0 as f64;
    let dy = y - y0 as f64;

    let sample = |sx: i64, sy: i64| -> f64 {
        if sx < 0 || sy < 0 || sx >= i64::from(image.width) || sy >= i64::from(image.height) {
            return 0.0;
        }
        let index = ((sy as u64 * u64::from(image.width) + sx as u64) * 3) as usize + channel;
        f64::from(image.rgb[index])
    };

    let top = sample(x0, y0) * (1.0 - dx) + sample(x1, y0) * dx;
    let bottom = sample(x0, y1) * (1.0 - dx) + sample(x1, y1) * dx;
    (top * (1.0 - dy) + bottom * dy).round().clamp(0.0, 255.0) as u8
}

fn validate_image(image: &NormalizedImage) -> Result<(), InferenceError> {
    let expected = (image.width as usize)
        .checked_mul(image.height as usize)
        .and_then(|pixels| pixels.checked_mul(3))
        .ok_or(InferenceError::UnexpectedShape)?;
    if image.width == 0 || image.height == 0 || image.rgb.len() != expected {
        return Err(InferenceError::UnexpectedShape);
    }
    Ok(())
}

/// Verifies the exact upstream checkpoint before ONNX Runtime parses it.
/// The model is intentionally not fetched by the application; operators provision it separately.
fn verify_model_artifact(model_path: &Path) -> Result<(), InferenceError> {
    let mut file = File::open(model_path).map_err(|_| InferenceError::ArtifactInvalid)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;

    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| InferenceError::ArtifactInvalid)?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .ok_or(InferenceError::ArtifactInvalid)?;
        hasher.update(&buffer[..read]);
    }

    let digest = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if size != SFACE_MODEL_SIZE_BYTES || digest != SFACE_MODEL_SHA256 {
        return Err(InferenceError::ArtifactInvalid);
    }
    Ok(())
}
