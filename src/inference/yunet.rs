//! YuNet face detector on ONNX Runtime (issue #24).
//!
//! Pre- and postprocessing follow OpenCV's `FaceDetectorYN` (4.x, `modules/objdetect/src/face_detect.cpp`):
//! - input is BGR, planar NCHW, float32 in 0..255, with no mean or scale;
//! - the image is letterboxed to 640×640 (scaled to fit, padded with zeros at the bottom and right);
//! - score = sqrt(clamp(cls) × clamp(obj)), per anchor, at strides 8, 16 and 32;
//! - boxes and landmarks decode from the anchor grid, followed by greedy NMS.
//!
//! Differences from OpenCV: NMS uses float IoU, not integer rectangles. The effect is
//! small, and it is documented in the README.
//!
//! Decoding is a pure function, so the coordinate and NMS logic is tested on hand-built
//! tensors. The ONNX session itself is tested against the vendored artifacts.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use image::{RgbImage, imageops::FilterType};
use ort::environment::Environment;
use ort::session::Session;
use ort::value::Tensor;

use crate::inference::{FaceDetection, FaceDetector, InferenceError, NormalizedBox};
use crate::ingress::NormalizedImage;

pub const INPUT_SIZE: usize = 640;
pub const STRIDES: [usize; 3] = [8, 16, 32];
/// Default confidence threshold, matching OpenCV's sample.
pub const DEFAULT_SCORE_THRESHOLD: f32 = 0.6;
/// Default NMS IoU threshold, matching OpenCV's sample.
pub const DEFAULT_NMS_THRESHOLD: f32 = 0.3;
pub const TOP_K: usize = 5000;
const INPUT_NAME: &str = "input";
const LANDMARK_COUNT: usize = 5;

/// Output tensor names, in the order the model produces them: cls, obj, bbox, kps for each stride.
pub const OUTPUT_NAMES: [&str; 12] = [
    "cls_8", "cls_16", "cls_32", "obj_8", "obj_16", "obj_32", "bbox_8", "bbox_16", "bbox_32",
    "kps_8", "kps_16", "kps_32",
];

/// Anchors per stride, for a 640×640 input: 80², 40² and 20².
pub fn anchors_for_stride(stride: usize) -> usize {
    let side = INPUT_SIZE / stride;
    side * side
}

/// How the original image was mapped into the model's input square.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Letterbox {
    /// Width of the scaled image inside the 640×640 square, in model pixels.
    pub content_w: f32,
    /// Height of the scaled image inside the 640×640 square, in model pixels.
    pub content_h: f32,
}

/// Builds the BGR NCHW input tensor and the letterbox geometry for an image.
pub fn preprocess(image: &NormalizedImage) -> Result<(Vec<f32>, Letterbox), InferenceError> {
    if image.width == 0
        || image.height == 0
        || image.rgb.len() != (image.width * image.height * 3) as usize
    {
        return Err(InferenceError::UnexpectedShape);
    }
    let (w, h) = (image.width as f32, image.height as f32);
    let scale = INPUT_SIZE as f32 / w.max(h);
    let content_w = (w * scale).round().clamp(1.0, INPUT_SIZE as f32) as u32;
    let content_h = (h * scale).round().clamp(1.0, INPUT_SIZE as f32) as u32;

    let rgb = RgbImage::from_raw(image.width, image.height, image.rgb.clone())
        .ok_or(InferenceError::UnexpectedShape)?;
    let resized = image::imageops::resize(&rgb, content_w, content_h, FilterType::Triangle);

    let plane = INPUT_SIZE * INPUT_SIZE;
    let mut tensor = vec![0.0f32; 3 * plane];
    for (x, y, px) in resized.enumerate_pixels() {
        let idx = y as usize * INPUT_SIZE + x as usize;
        // BGR channel order, as OpenCV's blobFromImage produces by default.
        tensor[idx] = f32::from(px[2]);
        tensor[plane + idx] = f32::from(px[1]);
        tensor[2 * plane + idx] = f32::from(px[0]);
    }
    Ok((
        tensor,
        Letterbox {
            content_w: content_w as f32,
            content_h: content_h as f32,
        },
    ))
}

/// Raw per-stride outputs, borrowed from the model's output tensors.
pub struct StrideOutputs<'a> {
    pub cls: [&'a [f32]; 3],
    pub obj: [&'a [f32]; 3],
    pub bbox: [&'a [f32]; 3],
    pub kps: [&'a [f32]; 3],
}

/// Checks that each slice has the length the grid requires.
fn check_lengths(outputs: &StrideOutputs<'_>) -> Result<(), InferenceError> {
    for (i, stride) in STRIDES.iter().enumerate() {
        let n = anchors_for_stride(*stride);
        if outputs.cls[i].len() != n
            || outputs.obj[i].len() != n
            || outputs.bbox[i].len() != n * 4
            || outputs.kps[i].len() != n * 10
        {
            return Err(InferenceError::UnexpectedShape);
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    /// Box in model pixels: x0, y0, x1, y1.
    model_box: [f32; 4],
    score: f32,
    /// Landmarks in model pixels.
    landmarks: [[f32; 2]; LANDMARK_COUNT],
}

fn iou(a: [f32; 4], b: [f32; 4]) -> f32 {
    let ix0 = a[0].max(b[0]);
    let iy0 = a[1].max(b[1]);
    let ix1 = a[2].min(b[2]);
    let iy1 = a[3].min(b[3]);
    let inter = (ix1 - ix0).max(0.0) * (iy1 - iy0).max(0.0);
    let area_a = (a[2] - a[0]).max(0.0) * (a[3] - a[1]).max(0.0);
    let area_b = (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    let union = area_a + area_b - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// Greedy NMS: keeps the highest-scoring box and drops any box whose IoU with a kept box exceeds `threshold`.
fn nms(mut candidates: Vec<Candidate>, threshold: f32) -> Vec<Candidate> {
    candidates.sort_by(|a, b| b.score.total_cmp(&a.score));
    candidates.truncate(TOP_K);
    let mut kept: Vec<Candidate> = Vec::new();
    for c in candidates {
        if kept
            .iter()
            .all(|k| iou(k.model_box, c.model_box) <= threshold)
        {
            kept.push(c);
        }
    }
    kept
}

/// Decodes the raw outputs into validated face detections, in normalised image coordinates.
pub fn decode(
    outputs: &StrideOutputs<'_>,
    geometry: Letterbox,
    score_threshold: f32,
    nms_threshold: f32,
) -> Result<Vec<FaceDetection>, InferenceError> {
    check_lengths(outputs)?;
    let mut candidates = Vec::new();
    for (i, &stride_usize) in STRIDES.iter().enumerate() {
        let stride = stride_usize as f32;
        let cols = INPUT_SIZE / stride_usize;
        let rows = INPUT_SIZE / stride_usize;
        for r in 0..rows {
            for c in 0..cols {
                let idx = r * cols + c;
                let cls = outputs.cls[i][idx].clamp(0.0, 1.0);
                let obj = outputs.obj[i][idx].clamp(0.0, 1.0);
                let score = (cls * obj).sqrt();
                if !score.is_finite() || score < score_threshold {
                    continue;
                }
                let b = &outputs.bbox[i][idx * 4..idx * 4 + 4];
                let cx = (c as f32 + b[0]) * stride;
                let cy = (r as f32 + b[1]) * stride;
                let w = b[2].exp() * stride;
                let h = b[3].exp() * stride;
                let (x0, y0) = (cx - w / 2.0, cy - h / 2.0);
                let k = &outputs.kps[i][idx * 10..idx * 10 + 10];
                let mut landmarks = [[0.0f32; 2]; LANDMARK_COUNT];
                for (n, lm) in landmarks.iter_mut().enumerate() {
                    lm[0] = (k[2 * n] + c as f32) * stride;
                    lm[1] = (k[2 * n + 1] + r as f32) * stride;
                }
                candidates.push(Candidate {
                    model_box: [x0, y0, x0 + w, y0 + h],
                    score,
                    landmarks,
                });
            }
        }
    }

    let kept = nms(candidates, nms_threshold);
    let mut out = Vec::with_capacity(kept.len());
    for c in kept {
        // Convert from model pixels to fractions of the original image, clipping to the frame.
        let to_frac_x = |v: f32| (v / geometry.content_w).clamp(0.0, 1.0);
        let to_frac_y = |v: f32| (v / geometry.content_h).clamp(0.0, 1.0);
        let [x0, y0, x1, y1] = c.model_box;
        let bbox =
            match NormalizedBox::new(to_frac_x(x0), to_frac_y(y0), to_frac_x(x1), to_frac_y(y1)) {
                Ok(b) => b,
                // Boxes that collapse to zero area after clipping (entirely outside the image) are dropped.
                Err(_) => continue,
            };
        let landmarks = c
            .landmarks
            .iter()
            .map(|p| [to_frac_x(p[0]), to_frac_y(p[1])])
            .collect();
        out.push(FaceDetection {
            bbox,
            score: c.score,
            landmarks,
        });
    }
    Ok(out)
}

/// YuNet detector backed by a loaded ONNX Runtime session.
pub struct YunetDetector {
    session: Mutex<Session>,
    score_threshold: f32,
    nms_threshold: f32,
}

/// ONNX Runtime can only be initialised once per process. The first runtime path wins, and
/// later calls must use the same path.
static ENVIRONMENT: OnceLock<(PathBuf, Result<Environment, InferenceError>)> = OnceLock::new();

fn environment(runtime_path: &Path) -> Result<&'static Environment, InferenceError> {
    let (loaded_path, env) = ENVIRONMENT.get_or_init(|| {
        let env = ort::init_from(runtime_path)
            .map_err(|_| InferenceError::Runtime)
            .and_then(|builder| {
                builder
                    .with_name("king-sparkon-eye")
                    .build()
                    .map_err(|_| InferenceError::Runtime)
            });
        (runtime_path.to_path_buf(), env)
    });
    if loaded_path != runtime_path {
        return Err(InferenceError::Runtime);
    }
    env.as_ref().map_err(|e| *e)
}

impl YunetDetector {
    /// Loads the runtime and the model, and checks the model's inputs and outputs against the expected shapes.
    ///
    /// Missing files return `ArtifactMissing`. Corrupt or incompatible files return `ArtifactInvalid`.
    /// A runtime that will not load, or a second runtime path in the same process, returns `Runtime`.
    pub fn load(model_path: &Path, runtime_path: &Path) -> Result<Self, InferenceError> {
        if !model_path.is_file() || !runtime_path.is_file() {
            return Err(InferenceError::ArtifactMissing);
        }
        let env = environment(runtime_path)?;
        let session = Session::builder(env)
            .map_err(|_| InferenceError::Runtime)?
            .commit_from_file(model_path)
            .map_err(|_| InferenceError::ArtifactInvalid)?;
        validate_io(&session)?;
        Ok(Self {
            session: Mutex::new(session),
            score_threshold: DEFAULT_SCORE_THRESHOLD,
            nms_threshold: DEFAULT_NMS_THRESHOLD,
        })
    }
}

fn validate_io(session: &Session) -> Result<(), InferenceError> {
    let inputs = session.inputs();
    if inputs.len() != 1 || inputs[0].name() != INPUT_NAME {
        return Err(InferenceError::ArtifactInvalid);
    }
    let input_shape = inputs[0]
        .dtype()
        .tensor_shape()
        .ok_or(InferenceError::ArtifactInvalid)?;
    if input_shape
        .iter()
        .copied()
        .ne([1i64, 3, INPUT_SIZE as i64, INPUT_SIZE as i64])
    {
        return Err(InferenceError::ArtifactInvalid);
    }
    let outputs = session.outputs();
    for name in OUTPUT_NAMES {
        if !outputs.iter().any(|o| o.name() == name) {
            return Err(InferenceError::ArtifactInvalid);
        }
    }
    Ok(())
}

impl FaceDetector for YunetDetector {
    fn detect(&self, image: &NormalizedImage) -> Result<Vec<FaceDetection>, InferenceError> {
        let (input, geometry) = preprocess(image)?;
        let tensor = Tensor::from_array(([1usize, 3, INPUT_SIZE, INPUT_SIZE], input))
            .map_err(|_| InferenceError::Runtime)?;
        let mut session = self.session.lock().map_err(|_| InferenceError::Runtime)?;
        let outputs = session
            .run(ort::inputs![INPUT_NAME => tensor])
            .map_err(|_| InferenceError::Runtime)?;

        let mut slices: Vec<&[f32]> = Vec::with_capacity(OUTPUT_NAMES.len());
        for name in OUTPUT_NAMES {
            let (_, data) = outputs[name]
                .try_extract_tensor::<f32>()
                .map_err(|_| InferenceError::UnexpectedShape)?;
            slices.push(data);
        }
        let stride_outputs = StrideOutputs {
            cls: [slices[0], slices[1], slices[2]],
            obj: [slices[3], slices[4], slices[5]],
            bbox: [slices[6], slices[7], slices[8]],
            kps: [slices[9], slices[10], slices[11]],
        };
        decode(
            &stride_outputs,
            geometry,
            self.score_threshold,
            self.nms_threshold,
        )
    }
}
