//! Stripe webhook signature verification (issue #141, verification slice only).
//!
//! This module checks that a `Stripe-Signature` header was produced by Stripe for the exact raw
//! request body. It does not parse events, persist deliveries, deduplicate, or change billing
//! state. A request must pass here before any of that, and that later work depends on durable
//! PostgreSQL storage (#113), so it is not implemented yet.
//!
//! The contract follows the official `stripe` Python SDK (`stripe/_webhook.py`, v16.1.0):
//! - the signed payload is `"{t}.{raw body}"`, using the raw bytes exactly as received;
//! - the signature is the lowercase hex HMAC-SHA256 of the signed payload, keyed with the secret;
//! - any `v1` entry may match; `v0` and other schemes are ignored;
//! - the default tolerance is 300 seconds.
//!
//! One deliberate difference: the SDK rejects only timestamps that are too old. This module also
//! rejects timestamps more than the tolerance in the future, because a legitimate Stripe clock
//! should never be that far ahead of ours. A skewed local clock can cause false rejections, so
//! the tolerance is configurable.

use ring::hmac;

use crate::config::SecretString;

pub const WEBHOOK_SECRET_PREFIX: &str = "whsec_";
pub const DEFAULT_WEBHOOK_TOLERANCE_SECS: i64 = 300;

/// Verification failures. Each variant has a fixed message and never echoes the header, the body
/// or the secret.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebhookError {
    #[error("webhook secret is missing or malformed")]
    SecretInvalid,
    #[error("Stripe-Signature header is missing")]
    SignatureMissing,
    #[error("Stripe-Signature header is malformed")]
    SignatureMalformed,
    #[error("no v1 signature matches the request body")]
    SignatureMismatch,
    #[error("webhook timestamp is outside the tolerance window")]
    TimestampOutOfTolerance,
}

impl WebhookError {
    pub fn code(&self) -> &'static str {
        match self {
            WebhookError::SecretInvalid => "WEBHOOK_SECRET_INVALID",
            WebhookError::SignatureMissing => "WEBHOOK_SIGNATURE_MISSING",
            WebhookError::SignatureMalformed => "WEBHOOK_SIGNATURE_MALFORMED",
            WebhookError::SignatureMismatch => "WEBHOOK_SIGNATURE_MISMATCH",
            WebhookError::TimestampOutOfTolerance => "WEBHOOK_TIMESTAMP_OUT_OF_TOLERANCE",
        }
    }
}

/// An endpoint signing secret. `Debug` is redacted because the inner `SecretString` redacts it.
#[derive(Debug)]
pub struct WebhookSecret(SecretString);

impl WebhookSecret {
    /// Accepts a `whsec_` secret with no whitespace or control characters.
    pub fn new(value: &str) -> Result<Self, WebhookError> {
        let valid = value.len() > WEBHOOK_SECRET_PREFIX.len()
            && value.starts_with(WEBHOOK_SECRET_PREFIX)
            && value.bytes().all(|b| b.is_ascii_graphic());
        if !valid {
            return Err(WebhookError::SecretInvalid);
        }
        Ok(Self(SecretString::new(value.to_owned())))
    }
}

/// The timestamp that a verified signature was made at, in Unix seconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerifiedSignature {
    pub timestamp: i64,
}

/// Verifies `header` against the exact `raw_body` bytes.
///
/// `now_unix` is passed in so the tolerance window can be tested. Signature is checked before the
/// timestamp, so a forged header never reveals whether its timestamp is recent.
pub fn verify_signature(
    secret: &WebhookSecret,
    header: Option<&str>,
    raw_body: &[u8],
    now_unix: i64,
    tolerance_secs: i64,
) -> Result<VerifiedSignature, WebhookError> {
    let header = match header {
        Some(value) if !value.is_empty() => value,
        _ => return Err(WebhookError::SignatureMissing),
    };
    let (timestamp, candidates) = parse_header(header)?;

    let key = hmac::Key::new(hmac::HMAC_SHA256, secret.0.expose().as_bytes());
    let mut signed = format!("{timestamp}.").into_bytes();
    signed.extend_from_slice(raw_body);

    // Check every candidate rather than stopping at the first match, so timing does not reveal
    // which entry matched.
    let mut matched = false;
    for candidate in candidates {
        if let Some(tag) = decode_hex(candidate) {
            matched |= hmac::verify(&key, &signed, &tag).is_ok();
        }
    }
    if !matched {
        return Err(WebhookError::SignatureMismatch);
    }

    let earliest = now_unix.saturating_sub(tolerance_secs);
    let latest = now_unix.saturating_add(tolerance_secs);
    if timestamp < earliest || timestamp > latest {
        return Err(WebhookError::TimestampOutOfTolerance);
    }
    Ok(VerifiedSignature { timestamp })
}

/// Splits the header into its timestamp and its `v1` candidates. Unknown schemes are ignored.
fn parse_header(header: &str) -> Result<(i64, Vec<&str>), WebhookError> {
    let mut timestamp: Option<&str> = None;
    let mut v1: Vec<&str> = Vec::new();
    for item in header.split(',') {
        let (key, value) = item
            .split_once('=')
            .ok_or(WebhookError::SignatureMalformed)?;
        match key {
            "t" => {
                if timestamp.replace(value).is_some() {
                    return Err(WebhookError::SignatureMalformed);
                }
            }
            "v1" => v1.push(value),
            _ => {}
        }
    }

    let raw = timestamp.ok_or(WebhookError::SignatureMalformed)?;
    if raw.is_empty() || raw.len() > 18 || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return Err(WebhookError::SignatureMalformed);
    }
    let parsed: i64 = raw.parse().map_err(|_| WebhookError::SignatureMalformed)?;
    if v1.is_empty() {
        return Err(WebhookError::SignatureMalformed);
    }
    Ok((parsed, v1))
}

/// Decodes a 64-character lowercase hex SHA-256 tag. Anything else returns `None`.
fn decode_hex(value: &str) -> Option<Vec<u8>> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    {
        return None;
    }
    Some(
        value
            .as_bytes()
            .chunks(2)
            .map(|pair| (nibble(pair[0]) << 4) | nibble(pair[1]))
            .collect(),
    )
}

fn nibble(byte: u8) -> u8 {
    match byte {
        b'0'..=b'9' => byte - b'0',
        _ => byte - b'a' + 10,
    }
}
