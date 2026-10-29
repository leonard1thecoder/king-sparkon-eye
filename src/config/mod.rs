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

pub const DEFAULT_VISION_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_VISION_MAX_RESPONSE_BYTES: usize = 64 * 1024;
pub const DEFAULT_VISION_MAX_RETRIES: u32 = 2;

/// Settings for the frontier vision provider (issue #25). Only used when the provider is enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisionConfig {
    pub base_url: String,
    pub model: String,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_retries: u32,
}

/// Runtime configuration for durable enrollment persistence and Kafka delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentRuntimeConfig {
    pub database_rest_url: String,
    pub database_service_role_key: SecretString,
    pub consent_verify_url: String,
    pub consent_service_key: SecretString,
    pub bootstrap_servers: String,
    pub security_protocol: String,
    pub sasl_mechanism: Option<String>,
    pub sasl_username: Option<SecretString>,
    pub sasl_password: Option<SecretString>,
    pub ssl_ca_location: Option<String>,
    pub ssl_certificate_location: Option<String>,
    pub ssl_key_location: Option<String>,
    pub ssl_key_password: Option<SecretString>,
    pub allow_localhost_http: bool,
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
    /// Frontier vision provider settings.
    pub vision: VisionConfig,
    /// Present only when the durable enrollment/outbox Kafka runtime is enabled.
    pub enrollment_runtime: Option<EnrollmentRuntimeConfig>,
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
pub const ENV_VISION_PROVIDER_BASE_URL: &str = "EYE_VISION_PROVIDER_BASE_URL";
pub const ENV_VISION_PROVIDER_MODEL: &str = "EYE_VISION_PROVIDER_MODEL";
pub const ENV_VISION_PROVIDER_TIMEOUT_MS: &str = "EYE_VISION_PROVIDER_TIMEOUT_MS";
pub const ENV_VISION_PROVIDER_MAX_RESPONSE_BYTES: &str = "EYE_VISION_PROVIDER_MAX_RESPONSE_BYTES";
pub const ENV_VISION_PROVIDER_MAX_RETRIES: &str = "EYE_VISION_PROVIDER_MAX_RETRIES";
pub const ENV_UPLOAD_MAX_BYTES: &str = "EYE_UPLOAD_MAX_BYTES";
pub const ENV_IMAGE_MAX_WIDTH: &str = "EYE_IMAGE_MAX_WIDTH";
pub const ENV_IMAGE_MAX_HEIGHT: &str = "EYE_IMAGE_MAX_HEIGHT";
pub const ENV_IMAGE_MAX_PIXELS: &str = "EYE_IMAGE_MAX_PIXELS";
pub const ENV_IMAGE_MAX_DECODE_ALLOC_BYTES: &str = "EYE_IMAGE_MAX_DECODE_ALLOC_BYTES";
pub const ENV_IMAGE_DECODE_TIMEOUT_MS: &str = "EYE_IMAGE_DECODE_TIMEOUT_MS";

pub const ENV_KAFKA_ENABLED: &str = "EYE_KAFKA_ENABLED";
pub const ENV_KAFKA_DATABASE_REST_URL: &str = "EYE_DATABASE_REST_URL";
pub const ENV_KAFKA_DATABASE_SERVICE_ROLE_KEY: &str = "EYE_DATABASE_SERVICE_ROLE_KEY";
pub const ENV_CONSENT_VERIFY_URL: &str = "EYE_CONSENT_VERIFY_URL";
pub const ENV_CONSENT_SERVICE_KEY: &str = "EYE_CONSENT_SERVICE_KEY";
pub const ENV_KAFKA_BOOTSTRAP_SERVERS: &str = "EYE_KAFKA_BOOTSTRAP_SERVERS";
pub const ENV_KAFKA_SECURITY_PROTOCOL: &str = "EYE_KAFKA_SECURITY_PROTOCOL";
pub const ENV_KAFKA_SASL_MECHANISM: &str = "EYE_KAFKA_SASL_MECHANISM";
pub const ENV_KAFKA_SASL_USERNAME: &str = "EYE_KAFKA_SASL_USERNAME";
pub const ENV_KAFKA_SASL_PASSWORD: &str = "EYE_KAFKA_SASL_PASSWORD";
pub const ENV_KAFKA_SSL_CA_LOCATION: &str = "EYE_KAFKA_SSL_CA_LOCATION";
pub const ENV_KAFKA_SSL_CERT_LOCATION: &str = "EYE_KAFKA_SSL_CERT_LOCATION";
pub const ENV_KAFKA_SSL_KEY_LOCATION: &str = "EYE_KAFKA_SSL_KEY_LOCATION";
pub const ENV_KAFKA_SSL_KEY_PASSWORD: &str = "EYE_KAFKA_SSL_KEY_PASSWORD";
pub const ENV_KAFKA_ALLOW_LOCALHOST_HTTP: &str = "EYE_KAFKA_ALLOW_LOCALHOST_HTTP";

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

        let enrollment_runtime = parse_enrollment_runtime(&lookup)?;

        let vision = VisionConfig {
            base_url: non_empty(lookup(ENV_VISION_PROVIDER_BASE_URL))
                .unwrap_or_else(|| "https://api.meta.ai/v1".to_owned()),
            model: non_empty(lookup(ENV_VISION_PROVIDER_MODEL))
                .unwrap_or_else(|| "muse-spark-1.3".to_owned()),
            timeout: Duration::from_millis(positive(
                &lookup,
                ENV_VISION_PROVIDER_TIMEOUT_MS,
                DEFAULT_VISION_TIMEOUT_MS,
            )?),
            max_response_bytes: positive(
                &lookup,
                ENV_VISION_PROVIDER_MAX_RESPONSE_BYTES,
                DEFAULT_VISION_MAX_RESPONSE_BYTES as u64,
            )? as usize,
            max_retries: non_negative_u32(
                &lookup,
                ENV_VISION_PROVIDER_MAX_RETRIES,
                DEFAULT_VISION_MAX_RETRIES,
            )?,
        };

        Ok(Self {
            bind_addr,
            log_filter,
            vision_provider_enabled,
            vision_provider_api_key,
            image_limits,
            vision,
            enrollment_runtime,
        })
    }
}


fn parse_enrollment_runtime<F>(lookup: &F) -> Result<Option<EnrollmentRuntimeConfig>, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    let enabled = match non_empty(lookup(ENV_KAFKA_ENABLED)).as_deref() {
        None | Some("false") | Some("0") => false,
        Some("true") | Some("1") => true,
        Some(_) => return Err(ConfigError::Invalid {
            name: ENV_KAFKA_ENABLED,
            reason: "expected true/false or 1/0".to_owned(),
        }),
    };
    if !enabled {
        return Ok(None);
    }

    let database_url = non_empty(lookup(ENV_KAFKA_DATABASE_REST_URL));
    let database_key = non_empty(lookup(ENV_KAFKA_DATABASE_SERVICE_ROLE_KEY));
    let consent_url = non_empty(lookup(ENV_CONSENT_VERIFY_URL));
    let consent_key = non_empty(lookup(ENV_CONSENT_SERVICE_KEY));
    let bootstrap = non_empty(lookup(ENV_KAFKA_BOOTSTRAP_SERVERS));
    let protocol = non_empty(lookup(ENV_KAFKA_SECURITY_PROTOCOL))
        .unwrap_or_else(|| "SSL".to_owned())
        .to_ascii_uppercase();
    let allow_localhost_http = match non_empty(lookup(ENV_KAFKA_ALLOW_LOCALHOST_HTTP)).as_deref() {
        None | Some("false") | Some("0") => false,
        Some("true") | Some("1") => true,
        Some(_) => return Err(ConfigError::Invalid {
            name: ENV_KAFKA_ALLOW_LOCALHOST_HTTP,
            reason: "expected true/false or 1/0".to_owned(),
        }),
    };

    if database_url.is_none() {
        return Err(ConfigError::Invalid { name: ENV_KAFKA_DATABASE_REST_URL, reason: "required when Kafka is enabled".to_owned() });
    }
    if database_key.as_deref().is_none_or(|secret| secret.len() < 32) {
        return Err(ConfigError::Invalid { name: ENV_KAFKA_DATABASE_SERVICE_ROLE_KEY, reason: "required and must contain at least 32 bytes".to_owned() });
    }
    if consent_url.is_none() {
        return Err(ConfigError::Invalid { name: ENV_CONSENT_VERIFY_URL, reason: "required when Kafka is enabled".to_owned() });
    }
    if consent_key.as_deref().is_none_or(|secret| secret.len() < 32) {
        return Err(ConfigError::Invalid { name: ENV_CONSENT_SERVICE_KEY, reason: "required and must contain at least 32 bytes".to_owned() });
    }
    if bootstrap.as_deref().is_none_or(|servers| servers.trim().is_empty() || servers.contains(' ') || servers.contains("://")) {
        return Err(ConfigError::Invalid { name: ENV_KAFKA_BOOTSTRAP_SERVERS, reason: "expected a comma-separated host:port list, not a URL".to_owned() });
    }
    if !matches!(protocol.as_str(), "SSL" | "SASL_SSL") {
        return Err(ConfigError::Invalid { name: ENV_KAFKA_SECURITY_PROTOCOL, reason: "TLS is required; use SSL or SASL_SSL".to_owned() });
    }

    let parsed_database = reqwest::Url::parse(database_url.as_deref().unwrap_or_default()).map_err(|_| ConfigError::Invalid {
        name: ENV_KAFKA_DATABASE_REST_URL, reason: "expected an absolute PostgREST /rest/v1 URL".to_owned()
    })?;
    if parsed_database.path().trim_end_matches('/') != "/rest/v1"
        || !parsed_database.username().is_empty()
        || parsed_database.password().is_some()
        || parsed_database.query().is_some()
        || parsed_database.fragment().is_some()
    {
        return Err(ConfigError::Invalid { name: ENV_KAFKA_DATABASE_REST_URL, reason: "must end in /rest/v1 and contain no credentials, query or fragment".to_owned() });
    }
    validate_runtime_url(
        consent_url.as_deref().unwrap_or_default(),
        "/api/internal/eye/consent/verify",
        ENV_CONSENT_VERIFY_URL,
        allow_localhost_http,
    )?;
    match parsed_database.scheme() {
        "https" => {}
        "http" if allow_localhost_http && is_loopback_host(parsed_database.host_str()) => {}
        _ => return Err(ConfigError::Invalid { name: ENV_KAFKA_DATABASE_REST_URL, reason: "HTTPS required except explicit loopback development".to_owned() }),
    }

    let sasl_mechanism = non_empty(lookup(ENV_KAFKA_SASL_MECHANISM));
    let sasl_username = non_empty(lookup(ENV_KAFKA_SASL_USERNAME));
    let sasl_password = non_empty(lookup(ENV_KAFKA_SASL_PASSWORD));
    if protocol == "SASL_SSL" && (sasl_mechanism.is_none() || sasl_username.is_none() || sasl_password.is_none()) {
        return Err(ConfigError::Invalid { name: ENV_KAFKA_SASL_USERNAME, reason: "SASL_SSL requires SASL mechanism, username and password".to_owned() });
    }
    if protocol == "SSL" && (sasl_mechanism.is_some() || sasl_username.is_some() || sasl_password.is_some()) {
        return Err(ConfigError::Invalid { name: ENV_KAFKA_SECURITY_PROTOCOL, reason: "SASL settings require SASL_SSL".to_owned() });
    }

    let ca = non_empty(lookup(ENV_KAFKA_SSL_CA_LOCATION));
    let cert = non_empty(lookup(ENV_KAFKA_SSL_CERT_LOCATION));
    let key = non_empty(lookup(ENV_KAFKA_SSL_KEY_LOCATION));
    let key_password = non_empty(lookup(ENV_KAFKA_SSL_KEY_PASSWORD));
    if cert.is_some() != key.is_some() {
        return Err(ConfigError::Invalid { name: ENV_KAFKA_SSL_CERT_LOCATION, reason: "client certificate and private key must be configured together".to_owned() });
    }

    Ok(Some(EnrollmentRuntimeConfig {
        database_rest_url: database_url.unwrap_or_default().trim_end_matches('/').to_owned(),
        database_service_role_key: SecretString::new(database_key.unwrap_or_default()),
        consent_verify_url: consent_url.unwrap_or_default(),
        consent_service_key: SecretString::new(consent_key.unwrap_or_default()),
        bootstrap_servers: bootstrap.unwrap_or_default(),
        security_protocol: protocol,
        sasl_mechanism,
        sasl_username: sasl_username.map(SecretString::new),
        sasl_password: sasl_password.map(SecretString::new),
        ssl_ca_location: ca,
        ssl_certificate_location: cert,
        ssl_key_location: key,
        ssl_key_password: key_password.map(SecretString::new),
        allow_localhost_http,
    }))
}

fn validate_runtime_url(
    raw: &str,
    expected_path: &str,
    name: &'static str,
    allow_localhost_http: bool,
) -> Result<(), ConfigError> {
    let url = reqwest::Url::parse(raw).map_err(|_| ConfigError::Invalid {
        name, reason: "expected an absolute HTTPS URL".to_owned()
    })?;
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some()
        || url.fragment().is_some() || url.path() != expected_path || url.as_str() != raw
    {
        return Err(ConfigError::Invalid { name, reason: "URL must have the exact configured path without credentials/query/fragment".to_owned() });
    }
    match url.scheme() {
        "https" => Ok(()),
        "http" if allow_localhost_http && is_loopback_host(url.host_str()) => Ok(()),
        _ => Err(ConfigError::Invalid { name, reason: "HTTPS required except explicit loopback development".to_owned() })
    }
}

fn is_loopback_host(host: Option<&str>) -> bool {
    matches!(host, Some("localhost" | "127.0.0.1" | "::1" | "[::1]"))
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

/// Reads a whole number that may be zero. Unset or blank uses the default.
fn non_negative_u32<F>(lookup: &F, name: &'static str, default: u32) -> Result<u32, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    match non_empty(lookup(name)) {
        None => Ok(default),
        Some(raw) => raw.parse::<u32>().map_err(|_| ConfigError::Invalid {
            name,
            reason: "expected a whole number of 0 or more".to_owned(),
        }),
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}
