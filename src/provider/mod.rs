//! Frontier vision provider interface (issue #25).
//!
//! The provider gives supplementary visual guidance only. It is not an authority for
//! image validation, face detection, identity or admission. Its output is parsed and
//! validated in `advisory`, and it cannot override deterministic results.
//!
//! Provider failures are distinct from poor photos. Each failure is a `ProviderError`,
//! so callers can report "guidance unavailable" without judging the photo.

pub mod muse;

use std::future::Future;
use std::io::Cursor;

use image::{ImageFormat, RgbImage};

use crate::ingress::NormalizedImage;

/// Controlled provider failures. Messages are fixed and carry no provider payloads or secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    #[error("vision provider is disabled")]
    Disabled,
    #[error("vision provider credentials are missing")]
    MissingCredentials,
    #[error("vision provider timed out")]
    Timeout,
    #[error("vision provider rate limit reached")]
    RateLimited,
    #[error("vision provider is temporarily unavailable")]
    Unavailable,
    #[error("vision provider rejected the request")]
    Rejected,
    #[error("vision provider response was invalid")]
    InvalidResponse,
    #[error("vision provider response exceeded the size limit")]
    ResponseTooLarge,
}

/// Encodes the normalised image as PNG. This drops any original metadata and sends only the pixels.
pub fn encode_png(image: &NormalizedImage) -> Result<Vec<u8>, ProviderError> {
    let rgb = RgbImage::from_raw(image.width, image.height, image.rgb.clone())
        .ok_or(ProviderError::Rejected)?;
    let mut out = Cursor::new(Vec::new());
    rgb.write_to(&mut out, ImageFormat::Png)
        .map_err(|_| ProviderError::Rejected)?;
    Ok(out.into_inner())
}

/// A vision provider. Implementations return the assistant's text, which the caller must validate.
pub trait VisionProvider: Send + Sync {
    fn advise(&self, png: Vec<u8>) -> impl Future<Output = Result<String, ProviderError>> + Send;
}

/// Deterministic provider for tests and CI. It never touches the network.
#[derive(Debug, Clone)]
pub struct MockProvider {
    pub response: Result<String, ProviderError>,
}

impl VisionProvider for MockProvider {
    fn advise(&self, _png: Vec<u8>) -> impl Future<Output = Result<String, ProviderError>> + Send {
        let response = self.response.clone();
        async move { response }
    }
}
