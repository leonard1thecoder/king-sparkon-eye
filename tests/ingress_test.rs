//! Image ingress tests. Fixtures are generated in memory. No real images are committed.

use std::io::Cursor;
use std::time::Duration;

use image::{DynamicImage, ImageFormat, RgbImage, RgbaImage};
use king_sparkon_eye::config::ImageLimits;
use king_sparkon_eye::error::{ApiError, ErrorCode};
use king_sparkon_eye::ingress::{
    IngressError, SupportedFormat, detect_format, ingest, ingest_with_timeout, read_bounded,
};

fn limits() -> ImageLimits {
    ImageLimits::default()
}

fn encode(img: &DynamicImage, format: ImageFormat) -> Vec<u8> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, format).expect("fixture encodes");
    out.into_inner()
}

fn solid_rgb(width: u32, height: u32) -> DynamicImage {
    DynamicImage::ImageRgb8(RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
    }))
}

fn png(width: u32, height: u32) -> Vec<u8> {
    encode(&solid_rgb(width, height), ImageFormat::Png)
}

fn jpeg(width: u32, height: u32) -> Vec<u8> {
    encode(&solid_rgb(width, height), ImageFormat::Jpeg)
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    let mut out = (data.len() as u32).to_be_bytes().to_vec();
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
    out
}

/// Inserts an EXIF APP1 segment with the given orientation tag right after the JPEG SOI.
fn jpeg_with_orientation(base: &[u8], orientation: u16) -> Vec<u8> {
    let mut tiff = vec![b'I', b'I', 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00];
    tiff.extend_from_slice(&[0x01, 0x00]); // one IFD entry
    tiff.extend_from_slice(&[0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00]); // tag 0x0112, SHORT, count 1
    tiff.extend_from_slice(&[
        (orientation & 0xFF) as u8,
        (orientation >> 8) as u8,
        0x00,
        0x00,
    ]);
    tiff.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // no next IFD
    let mut payload = b"Exif\0\0".to_vec();
    payload.extend_from_slice(&tiff);
    let length = (payload.len() + 2) as u16;
    let mut app1 = vec![0xFF, 0xE1, (length >> 8) as u8, (length & 0xFF) as u8];
    app1.extend_from_slice(&payload);
    let mut out = base[..2].to_vec();
    out.extend_from_slice(&app1);
    out.extend_from_slice(&base[2..]);
    out
}

// ---- valid inputs ----

#[test]
fn valid_png_is_accepted_and_normalized_to_rgb() {
    let out = ingest(&png(8, 6), &limits()).expect("valid png");
    assert_eq!(out.format, SupportedFormat::Png);
    assert_eq!((out.width, out.height), (8, 6));
    assert_eq!(out.rgb.len(), 8 * 6 * 3);
}

#[test]
fn valid_jpeg_is_accepted() {
    let out = ingest(&jpeg(16, 12), &limits()).expect("valid jpeg");
    assert_eq!(out.format, SupportedFormat::Jpeg);
    assert_eq!((out.width, out.height), (16, 12));
    assert_eq!(out.rgb.len(), 16 * 12 * 3);
}

#[test]
fn rgba_png_is_normalized_to_three_channels() {
    let rgba = DynamicImage::ImageRgba8(RgbaImage::from_pixel(5, 5, image::Rgba([10, 20, 30, 40])));
    let out = ingest(&encode(&rgba, ImageFormat::Png), &limits()).unwrap();
    assert_eq!(out.rgb.len(), 5 * 5 * 3);
}

#[test]
fn exif_orientation_is_applied() {
    // A 4x2 image tagged orientation 6 (rotate 90 degrees clockwise) must come out as 2x4.
    let tagged = jpeg_with_orientation(&jpeg(4, 2), 6);
    let out = ingest(&tagged, &limits()).expect("oriented jpeg");
    assert_eq!((out.width, out.height), (2, 4));
}

// ---- rejection: empty, malformed, spoofed, unsupported ----

#[test]
fn empty_upload_is_malformed() {
    assert_eq!(ingest(&[], &limits()), Err(IngressError::Malformed));
}

#[test]
fn unknown_bytes_are_unsupported() {
    assert_eq!(
        ingest(b"just some text, not an image", &limits()),
        Err(IngressError::UnsupportedFormat)
    );
}

#[test]
fn spoofed_jpeg_signature_with_garbage_is_malformed() {
    // Passes the JPEG signature check but has no end-of-image marker.
    let spoofed = [&[0xFF, 0xD8, 0xFF][..], b"garbage garbage garbage"].concat();
    assert_eq!(ingest(&spoofed, &limits()), Err(IngressError::Malformed));
}

#[test]
fn png_bytes_named_as_jpeg_are_still_detected_by_signature() {
    // The pipeline has no filename or MIME input. Detection uses the bytes alone.
    assert_eq!(detect_format(&png(2, 2)).unwrap(), SupportedFormat::Png);
}

#[test]
fn gif_and_webp_are_unsupported() {
    let gif = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";
    let webp = b"RIFF\x24\x00\x00\x00WEBPVP8 \x18\x00\x00\x00\x30\x01\x00\x9d\x01\x2a\x01\x00\x01\x00\x00\x00";
    assert_eq!(ingest(gif, &limits()), Err(IngressError::UnsupportedFormat));
    assert_eq!(
        ingest(webp, &limits()),
        Err(IngressError::UnsupportedFormat)
    );
}

// ---- rejection: truncation and structure ----

#[test]
fn truncated_png_is_malformed() {
    let full = png(8, 8);
    let truncated = &full[..full.len() - 6];
    assert_eq!(ingest(truncated, &limits()), Err(IngressError::Malformed));
}

#[test]
fn truncated_jpeg_is_malformed() {
    let full = jpeg(8, 8);
    let truncated = &full[..full.len() - 2];
    assert_eq!(ingest(truncated, &limits()), Err(IngressError::Malformed));
}

#[test]
fn png_with_trailing_bytes_after_iend_is_malformed() {
    let mut bytes = png(4, 4);
    bytes.extend_from_slice(b"extra");
    assert_eq!(ingest(&bytes, &limits()), Err(IngressError::Malformed));
}

#[test]
fn animated_png_is_unsupported() {
    // Insert an acTL chunk right after IHDR. That makes the file an APNG.
    let base = png(4, 4);
    let ihdr_end = 8 + 4 + 4 + 13 + 4;
    let actl = png_chunk(b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]);
    let mut animated = base[..ihdr_end].to_vec();
    animated.extend_from_slice(&actl);
    animated.extend_from_slice(&base[ihdr_end..]);
    assert_eq!(
        ingest(&animated, &limits()),
        Err(IngressError::UnsupportedFormat)
    );
}

// ---- limits ----

#[test]
fn oversized_upload_is_rejected_before_decoding() {
    let bytes = jpeg(32, 32);
    let tight = ImageLimits {
        max_upload_bytes: bytes.len() - 1,
        ..limits()
    };
    assert_eq!(ingest(&bytes, &tight), Err(IngressError::TooLarge));
}

#[test]
fn read_bounded_stops_at_the_byte_limit() {
    let data = [7u8; 100];
    assert_eq!(read_bounded(&data[..], 10), Err(IngressError::TooLarge));
    assert_eq!(read_bounded(&data[..], 100).unwrap().len(), 100);
}

#[test]
fn extreme_width_is_rejected_from_header_alone() {
    let wide = png(5000, 1);
    assert_eq!(
        ingest(&wide, &limits()),
        Err(IngressError::DimensionsExceeded)
    );
}

#[test]
fn extreme_height_is_rejected() {
    let tall = jpeg(1, 5000);
    assert_eq!(
        ingest(&tall, &limits()),
        Err(IngressError::DimensionsExceeded)
    );
}

#[test]
fn pixel_count_limit_is_enforced_even_when_each_side_is_allowed() {
    let fixture = png(100, 100);
    let tight = ImageLimits {
        max_pixels: 5_000,
        ..limits()
    };
    assert_eq!(
        ingest(&fixture, &tight),
        Err(IngressError::DimensionsExceeded)
    );
}

#[test]
fn decode_allocation_limit_is_enforced() {
    // 1 byte of allowed decode memory cannot hold any real image.
    let fixture = png(64, 64);
    let tight = ImageLimits {
        max_decode_alloc_bytes: 1,
        ..limits()
    };
    assert_eq!(
        ingest(&fixture, &tight),
        Err(IngressError::DimensionsExceeded)
    );
}

#[tokio::test]
async fn decode_timeout_returns_a_timeout_error() {
    // Large enough that decoding is far slower than one nanosecond.
    let fixture = png(2000, 2000);
    let tight = ImageLimits {
        decode_timeout: Duration::from_nanos(1),
        ..limits()
    };
    assert_eq!(
        ingest_with_timeout(fixture, tight).await,
        Err(IngressError::DecodeTimeout)
    );
}

#[tokio::test]
async fn async_ingest_succeeds_within_timeout() {
    let out = ingest_with_timeout(png(8, 8), limits())
        .await
        .expect("within timeout");
    assert_eq!((out.width, out.height), (8, 8));
}

// ---- stable error mapping ----

#[test]
fn every_ingress_error_maps_to_a_documented_code_without_decoder_text() {
    let cases = [
        (IngressError::Malformed, ErrorCode::ImageMalformed),
        (IngressError::TooLarge, ErrorCode::PayloadTooLarge),
        (
            IngressError::UnsupportedFormat,
            ErrorCode::UnsupportedMediaType,
        ),
        (
            IngressError::DimensionsExceeded,
            ErrorCode::ImageDimensionsExceeded,
        ),
        (IngressError::DecodeTimeout, ErrorCode::ServiceUnavailable),
        (IngressError::Internal, ErrorCode::Internal),
    ];
    for (err, expected) in cases {
        let api: ApiError = err.into();
        assert_eq!(api.code(), expected, "{err:?}");
        let message = api.body().message;
        for leaked in ["image::", "decoder", "png", "jpeg", "crc", "chunk"] {
            assert!(
                !message.to_lowercase().contains(leaked),
                "decoder detail `{leaked}` leaked: {message}"
            );
        }
    }
}

#[test]
fn ingress_error_messages_are_fixed_and_contain_no_paths() {
    for err in [
        IngressError::Malformed,
        IngressError::TooLarge,
        IngressError::UnsupportedFormat,
        IngressError::DimensionsExceeded,
        IngressError::DecodeTimeout,
        IngressError::Internal,
    ] {
        let message = ApiError::from(err).body().message;
        assert!(
            !message.contains('/') && !message.contains('\\'),
            "{message}"
        );
    }
}
