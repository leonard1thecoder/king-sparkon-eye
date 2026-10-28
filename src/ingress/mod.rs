//! Secure image ingress (issue #23).
//!
//! Every uploaded image passes through this pipeline before any model call. The
//! pipeline:
//! 1. enforces a byte limit while reading, so memory stays bounded;
//! 2. ignores the client filename and MIME type, and detects the format from file signatures;
//! 3. checks the file structure (JPEG end marker; PNG chunk walk, rejecting APNG and truncation);
//! 4. checks header dimensions and pixel count before any pixel buffer is allocated;
//! 5. decodes under an allocation limit, with a timeout;
//! 6. applies EXIF orientation and normalises to 8-bit RGB.
//!
//! Only JPEG and static PNG are supported. Image bytes and decoder details never
//! leave this module.

use std::io::{Cursor, Read};

use image::{
    DynamicImage, ImageDecoder, ImageError, ImageFormat, ImageReader, Limits, metadata::Orientation,
};

use crate::config::ImageLimits;
use crate::error::ApiError;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const JPEG_SOI: [u8; 3] = [0xFF, 0xD8, 0xFF];
const JPEG_EOI: [u8; 2] = [0xFF, 0xD9];

/// Formats accepted by ingress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupportedFormat {
    Jpeg,
    Png,
}

/// Decoded, orientation-corrected image in 8-bit RGB, ready for inference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedImage {
    pub format: SupportedFormat,
    pub width: u32,
    pub height: u32,
    /// Row-major RGB bytes, `width * height * 3` long.
    pub rgb: Vec<u8>,
}

/// Why an upload was rejected. Variants carry no decoder text, so none can leak to clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum IngressError {
    #[error("uploaded image is malformed or truncated")]
    Malformed,
    #[error("upload exceeds the allowed size")]
    TooLarge,
    #[error("file type is not supported")]
    UnsupportedFormat,
    #[error("image dimensions exceed the allowed limits")]
    DimensionsExceeded,
    #[error("image processing timed out")]
    DecodeTimeout,
    #[error("internal error")]
    Internal,
}

impl From<IngressError> for ApiError {
    fn from(err: IngressError) -> Self {
        match err {
            IngressError::Malformed => ApiError::ImageMalformed,
            IngressError::TooLarge => ApiError::PayloadTooLarge,
            IngressError::UnsupportedFormat => ApiError::UnsupportedMediaType,
            IngressError::DimensionsExceeded => ApiError::ImageDimensionsExceeded,
            IngressError::DecodeTimeout => ApiError::ImageDecodeTimeout,
            IngressError::Internal => ApiError::Internal,
        }
    }
}

/// Reads at most `max` bytes and stops as soon as the limit is exceeded.
pub fn read_bounded(reader: impl Read, max: usize) -> Result<Vec<u8>, IngressError> {
    let mut buf = Vec::new();
    reader
        .take(max as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|_| IngressError::Malformed)?;
    if buf.len() > max {
        return Err(IngressError::TooLarge);
    }
    Ok(buf)
}

/// Detects the format from file signatures. Client-supplied names and MIME types are never consulted.
pub fn detect_format(bytes: &[u8]) -> Result<SupportedFormat, IngressError> {
    if bytes.starts_with(&JPEG_SOI) {
        Ok(SupportedFormat::Jpeg)
    } else if bytes.starts_with(&PNG_SIGNATURE) {
        Ok(SupportedFormat::Png)
    } else {
        Err(IngressError::UnsupportedFormat)
    }
}

/// Checks file structure before decoding. Rejects truncated files and animated PNG.
pub fn check_structure(bytes: &[u8], format: SupportedFormat) -> Result<(), IngressError> {
    match format {
        SupportedFormat::Jpeg => {
            if bytes.len() >= 4 && bytes.ends_with(&JPEG_EOI) {
                Ok(())
            } else {
                Err(IngressError::Malformed)
            }
        }
        SupportedFormat::Png => check_png_chunks(bytes),
    }
}

fn check_png_chunks(bytes: &[u8]) -> Result<(), IngressError> {
    let mut pos = PNG_SIGNATURE.len();
    let mut first = true;
    loop {
        let header_end = pos.checked_add(8).ok_or(IngressError::Malformed)?;
        if header_end > bytes.len() {
            return Err(IngressError::Malformed);
        }
        let length =
            u32::from_be_bytes([bytes[pos], bytes[pos + 1], bytes[pos + 2], bytes[pos + 3]])
                as usize;
        let kind = &bytes[pos + 4..pos + 8];
        if first && kind != b"IHDR" {
            return Err(IngressError::Malformed);
        }
        first = false;
        // Chunk = length + type + data + CRC.
        let chunk_end = header_end
            .checked_add(length)
            .and_then(|v| v.checked_add(4))
            .ok_or(IngressError::Malformed)?;
        if chunk_end > bytes.len() {
            return Err(IngressError::Malformed);
        }
        if kind == b"acTL" {
            // Animation control chunk: animated PNG is out of scope.
            return Err(IngressError::UnsupportedFormat);
        }
        if kind == b"IEND" {
            return if chunk_end == bytes.len() {
                Ok(())
            } else {
                Err(IngressError::Malformed)
            };
        }
        pos = chunk_end;
    }
}

/// Runs the full pipeline on an in-memory upload.
pub fn ingest(bytes: &[u8], limits: &ImageLimits) -> Result<NormalizedImage, IngressError> {
    if bytes.is_empty() {
        return Err(IngressError::Malformed);
    }
    if bytes.len() > limits.max_upload_bytes {
        return Err(IngressError::TooLarge);
    }
    let format = detect_format(bytes)?;
    check_structure(bytes, format)?;
    let image_format = match format {
        SupportedFormat::Jpeg => ImageFormat::Jpeg,
        SupportedFormat::Png => ImageFormat::Png,
    };

    // Header-only dimension check. No pixel buffer is allocated before this passes.
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), image_format)
        .into_dimensions()
        .map_err(|_| IngressError::Malformed)?;
    if width == 0 || height == 0 {
        return Err(IngressError::Malformed);
    }
    if width > limits.max_width
        || height > limits.max_height
        || u64::from(width) * u64::from(height) > limits.max_pixels
    {
        return Err(IngressError::DimensionsExceeded);
    }

    let mut decode_limits = Limits::default();
    decode_limits.max_image_width = Some(limits.max_width);
    decode_limits.max_image_height = Some(limits.max_height);
    decode_limits.max_alloc = Some(limits.max_decode_alloc_bytes);

    let mut reader = ImageReader::with_format(Cursor::new(bytes), image_format);
    reader.limits(decode_limits);
    let mut decoder = reader.into_decoder().map_err(map_decode_error)?;
    // A bad EXIF block should not reject an otherwise valid photo, so treat it as "no transform".
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(map_decode_error)?;
    image.apply_orientation(orientation);

    let rgb = image.to_rgb8();
    let (width, height) = rgb.dimensions();
    Ok(NormalizedImage {
        format,
        width,
        height,
        rgb: rgb.into_raw(),
    })
}

fn map_decode_error(err: ImageError) -> IngressError {
    match err {
        ImageError::Limits(_) => IngressError::DimensionsExceeded,
        _ => IngressError::Malformed,
    }
}

/// Runs [`ingest`] on a blocking thread with a time limit.
///
/// A timed-out decode keeps running until it returns, because a blocking thread cannot be
/// cancelled. The byte, dimension and allocation limits are what bound the work.
pub async fn ingest_with_timeout(
    bytes: Vec<u8>,
    limits: ImageLimits,
) -> Result<NormalizedImage, IngressError> {
    let timeout = limits.decode_timeout;
    let handle = tokio::task::spawn_blocking(move || ingest(&bytes, &limits));
    match tokio::time::timeout(timeout, handle).await {
        Ok(Ok(result)) => result,
        Ok(Err(_join_error)) => Err(IngressError::Internal),
        Err(_elapsed) => Err(IngressError::DecodeTimeout),
    }
}
