//! Environment-based configuration.
//!
//! Secrets are wrapped in [`SecretString`], whose `Debug` output is redacted,
//! and configuration errors name the variable but never echo its value.

use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

pub const DEFAULT_BIND_ADDR: &str = "0.0.0.0:8080";
pub const DEFAULT_LOG_FILTER: &str = "info";

/// A string value that must never appear in logs or debug output.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Exposes the raw secret. Only call this where the value is actually sent to a dependency.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretString([REDACTED])")
    }
}

pub const DEFAULT_UPLOAD_MAX_BYTES: usize = 10 * 1024 * 1024;
pub const DEFAULT_IMAGE_MAX_WIDTH: u32 = 4096;
pub const DEFAULT_IMAGE_MAX_HEIGHT: u32 = 4096;
pub const DEFAULT_IMAGE_MAX_PIXELS: u64 = 16_777_216;
pub const DEFAULT_IMAGE_MAX_DECODE_ALLOC_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_IMAGE_DECODE_TIMEOUT_MS: u64 = 5_000;

/// Resource limits for uploaded images. Enforced before any model call (issue #23).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLimits {
    pub max_upload_bytes: usize,
    pub max_width: u32,
    pub max_height: u32,
    pub max_pixels: u64,
    pub max_decode_alloc_bytes: u64,
    pub decode_timeout: Duration,
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            max_upload_bytes: DEFAULT_UPLOAD_MAX_BYTES,
            max_width: DEFAULT_IMAGE_MAX_WIDTH,
            max_height: DEFAULT_IMAGE_MAX_HEIGHT,
            max_pixels: DEFAULT_IMAGE_MAX_PIXELS,
            max_decode_alloc_bytes: DEFAULT_IMAGE_MAX_DECODE_ALLOC_BYTES,
            decode_timeout: Duration::from_millis(DEFAULT_IMAGE_DECODE_TIMEOUT_MS),
        }
    }
}

/// Runtime configuration for the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub log_filter: String,
    /// Whether the optional frontier vision provider (issue #25) is enabled.
    pub vision_provider_enabled: bool,
    /// Credential for the vision provider. Optional; required only when the provider is enabled.
    pub vision_provider_api_key: Option<SecretString>,
    /// Upload and decode limits for image ingress.
    pub image_limits: ImageLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("environment variable {name} has an invalid value: {reason}")]
    Invalid { name: &'static str, reason: String },
}

pub const ENV_BIND_ADDR: &str = "EYE_BIND_ADDR";
pub const ENV_LOG_FILTER: &str = "EYE_LOG_FILTER";
pub const ENV_VISION_PROVIDER_ENABLED: &str = "EYE_VISION_PROVIDER_ENABLED";
pub const ENV_VISION_PROVIDER_API_KEY: &str = "EYE_VISION_PROVIDER_API_KEY";
pub const ENV_UPLOAD_MAX_BYTES: &str = "EYE_UPLOAD_MAX_BYTES";
pub const ENV_IMAGE_MAX_WIDTH: &str = "EYE_IMAGE_MAX_WIDTH";
pub const ENV_IMAGE_MAX_HEIGHT: &str = "EYE_IMAGE_MAX_HEIGHT";
pub const ENV_IMAGE_MAX_PIXELS: &str = "EYE_IMAGE_MAX_PIXELS";
pub const ENV_IMAGE_MAX_DECODE_ALLOC_BYTES: &str = "EYE_IMAGE_MAX_DECODE_ALLOC_BYTES";
pub const ENV_IMAGE_DECODE_TIMEOUT_MS: &str = "EYE_IMAGE_DECODE_TIMEOUT_MS";

impl Config {
    /// Loads configuration from the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Loads configuration from an arbitrary lookup function (used by tests).
    pub fn from_lookup<F>(lookup: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let bind_raw =
            non_empty(lookup(ENV_BIND_ADDR)).unwrap_or_else(|| DEFAULT_BIND_ADDR.to_owned());
        let bind_addr: SocketAddr = bind_raw.parse().map_err(|_| ConfigError::Invalid {
            name: ENV_BIND_ADDR,
            reason: "expected a socket address such as 0.0.0.0:8080".to_owned(),
        })?;

        let log_filter =
            non_empty(lookup(ENV_LOG_FILTER)).unwrap_or_else(|| DEFAULT_LOG_FILTER.to_owned());

        let vision_provider_enabled =
            match non_empty(lookup(ENV_VISION_PROVIDER_ENABLED)).as_deref() {
                None | Some("false") | Some("0") => false,
                Some("true") | Some("1") => true,
                Some(_) => {
                    return Err(ConfigError::Invalid {
                        name: ENV_VISION_PROVIDER_ENABLED,
                        reason: "expected true/false or 1/0".to_owned(),
                    });
                }
            };

        let vision_provider_api_key =
            non_empty(lookup(ENV_VISION_PROVIDER_API_KEY)).map(SecretString::new);

        let defaults = ImageLimits::default();
        let image_limits = ImageLimits {
            max_upload_bytes: positive(
                &lookup,
                ENV_UPLOAD_MAX_BYTES,
                defaults.max_upload_bytes as u64,
            )? as usize,
            max_width: positive(&lookup, ENV_IMAGE_MAX_WIDTH, u64::from(defaults.max_width))?
                as u32,
            max_height: positive(
                &lookup,
                ENV_IMAGE_MAX_HEIGHT,
                u64::from(defaults.max_height),
            )? as u32,
            max_pixels: positive(&lookup, ENV_IMAGE_MAX_PIXELS, defaults.max_pixels)?,
            max_decode_alloc_bytes: positive(
                &lookup,
                ENV_IMAGE_MAX_DECODE_ALLOC_BYTES,
                defaults.max_decode_alloc_bytes,
            )?,
            decode_timeout: Duration::from_millis(positive(
                &lookup,
                ENV_IMAGE_DECODE_TIMEOUT_MS,
                DEFAULT_IMAGE_DECODE_TIMEOUT_MS,
            )?),
        };

        Ok(Self {
            bind_addr,
            log_filter,
            vision_provider_enabled,
            vision_provider_api_key,
            image_limits,
        })
    }
}

/// Reads a positive integer limit. Unset or blank uses the default. Zero and non-numbers are rejected.
fn positive<F>(lookup: &F, name: &'static str, default: u64) -> Result<u64, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    match non_empty(lookup(name)) {
        None => Ok(default),
        Some(raw) => match raw.parse::<u64>() {
            Ok(value) if value > 0 => Ok(value),
            _ => Err(ConfigError::Invalid {
                name,
                reason: "expected a positive whole number".to_owned(),
            }),
        },
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}
