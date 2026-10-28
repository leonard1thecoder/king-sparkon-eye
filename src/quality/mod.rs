//! Objective image-quality checks (issue #24).
//!
//! These checks are deterministic and do not use a model. Each one returns a
//! [`Verdict`]. A measurement that cannot be taken is `Unknown`, never `Pass`.
//!
//! The default thresholds are uncalibrated starting points. They must be evaluated
//! against a consented dataset (issues #8 and #17) before being relied on.

use crate::inference::FaceDetection;
use crate::ingress::NormalizedImage;

/// Outcome of one check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    Fail,
    /// The measurement could not be taken, for example because no face was detected.
    Unknown,
}

/// Thresholds for the objective checks. Defaults are uncalibrated.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualityThresholds {
    /// Minimum variance of the Laplacian over 0-255 luma. Lower means blurrier.
    pub min_sharpness: f64,
    /// Mean luma range that counts as acceptable exposure, on a 0-255 scale.
    pub min_mean_luma: f32,
    pub max_mean_luma: f32,
    /// Maximum fraction of pixels at or near black (<= 5) or white (>= 250).
    pub max_clipped_fraction: f32,
    /// Face width as a fraction of image width, accepted range (inclusive).
    pub min_face_width_fraction: f32,
    pub max_face_width_fraction: f32,
    /// Minimum distance from each frame edge to the face box, as a fraction of image size.
    pub frame_margin: f32,
}

impl Default for QualityThresholds {
    fn default() -> Self {
        Self {
            min_sharpness: 100.0,
            min_mean_luma: 40.0,
            max_mean_luma: 220.0,
            max_clipped_fraction: 0.05,
            min_face_width_fraction: 0.25,
            max_face_width_fraction: 0.90,
            frame_margin: 0.02,
        }
    }
}

/// Full objective report. Raw values are `None` when they could not be measured.
#[derive(Debug, Clone, PartialEq)]
pub struct QualityReport {
    pub sharpness: Verdict,
    pub sharpness_value: Option<f64>,
    pub exposure: Verdict,
    pub mean_luma: Option<f32>,
    pub clipped_fraction: Option<f32>,
    pub face_size: Verdict,
    pub face_width_fraction: Option<f32>,
    pub framing: Verdict,
}

/// Luma (0-255) for each pixel, from an 8-bit RGB buffer. Uses ITU-R BT.601 weights.
pub fn luma(image: &NormalizedImage) -> Vec<f32> {
    image
        .rgb
        .chunks_exact(3)
        .map(|px| 0.299 * f32::from(px[0]) + 0.587 * f32::from(px[1]) + 0.114 * f32::from(px[2]))
        .collect()
}

/// Variance of the 4-neighbour Laplacian over interior pixels. `None` for images smaller than 3x3.
pub fn sharpness(image: &NormalizedImage) -> Option<f64> {
    let (w, h) = (image.width as usize, image.height as usize);
    if w < 3 || h < 3 {
        return None;
    }
    let l = luma(image);
    let mut sum = 0.0f64;
    let mut sum_sq = 0.0f64;
    let mut count = 0u64;
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let c = f64::from(l[y * w + x]);
            let lap = f64::from(l[y * w + x - 1])
                + f64::from(l[y * w + x + 1])
                + f64::from(l[(y - 1) * w + x])
                + f64::from(l[(y + 1) * w + x])
                - 4.0 * c;
            sum += lap;
            sum_sq += lap * lap;
            count += 1;
        }
    }
    let n = count as f64;
    let mean = sum / n;
    Some(sum_sq / n - mean * mean)
}

/// Mean luma and fraction of clipped pixels. `None` for an empty image.
pub fn exposure(image: &NormalizedImage) -> Option<(f32, f32)> {
    let l = luma(image);
    if l.is_empty() {
        return None;
    }
    let n = l.len() as f32;
    let mean = l.iter().sum::<f32>() / n;
    let clipped = l.iter().filter(|v| **v <= 5.0 || **v >= 250.0).count() as f32 / n;
    Some((mean, clipped))
}

fn in_range(value: f32, min: f32, max: f32) -> Verdict {
    if (min..=max).contains(&value) {
        Verdict::Pass
    } else {
        Verdict::Fail
    }
}

/// Measures every objective check. Face checks are `Unknown` when `face` is `None`.
/// The caller passes the face only when there was exactly one.
pub fn measure(
    image: &NormalizedImage,
    face: Option<&FaceDetection>,
    t: &QualityThresholds,
) -> QualityReport {
    let sharpness_value = sharpness(image);
    let sharpness_verdict = match sharpness_value {
        Some(v) if v >= t.min_sharpness => Verdict::Pass,
        Some(_) => Verdict::Fail,
        None => Verdict::Unknown,
    };

    let exposure_values = exposure(image);
    let (exposure_verdict, mean_luma, clipped_fraction) = match exposure_values {
        Some((mean, clipped)) => {
            let ok = in_range(mean, t.min_mean_luma, t.max_mean_luma) == Verdict::Pass
                && clipped <= t.max_clipped_fraction;
            (
                if ok { Verdict::Pass } else { Verdict::Fail },
                Some(mean),
                Some(clipped),
            )
        }
        None => (Verdict::Unknown, None, None),
    };

    let (face_size, face_width_fraction, framing) = match face {
        Some(f) => {
            let width = f.bbox.width();
            let size = in_range(width, t.min_face_width_fraction, t.max_face_width_fraction);
            let inside = f.bbox.x0 >= t.frame_margin
                && f.bbox.y0 >= t.frame_margin
                && f.bbox.x1 <= 1.0 - t.frame_margin
                && f.bbox.y1 <= 1.0 - t.frame_margin;
            (
                size,
                Some(width),
                if inside { Verdict::Pass } else { Verdict::Fail },
            )
        }
        None => (Verdict::Unknown, None, Verdict::Unknown),
    };

    QualityReport {
        sharpness: sharpness_verdict,
        sharpness_value,
        exposure: exposure_verdict,
        mean_luma,
        clipped_fraction,
        face_size,
        face_width_fraction,
        framing,
    }
}
