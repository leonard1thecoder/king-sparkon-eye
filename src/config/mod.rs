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
    /// OAuth runtime is disabled unless every required server-side setting is supplied.
    pub oauth: Option<OAuthRuntimeConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthRuntimeConfig {
    pub google_client_id: String,
    pub google_redirect_url: String,
    pub google_client_secret: SecretString,
    /// PostgREST URL for an Eye-owned PostgreSQL database (normally ending in /rest/v1).
    pub database_rest_url: String,
    pub database_service_role_key: SecretString,
    pub callback_urls: String,
    pub allow_localhost_http: bool,
    pub exchange_secret: SecretString,
    pub jwt_signing_secret: SecretString,
    pub jwt_kid: String,
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
pub const ENV_OAUTH_GOOGLE_CLIENT_ID: &str = "EYE_GOOGLE_CLIENT_ID";
pub const ENV_OAUTH_GOOGLE_CLIENT_SECRET: &str = "EYE_GOOGLE_CLIENT_SECRET";
pub const ENV_OAUTH_GOOGLE_REDIRECT_URL: &str = "EYE_GOOGLE_REDIRECT_URL";
pub const ENV_OAUTH_DATABASE_REST_URL: &str = "EYE_DATABASE_REST_URL";
pub const ENV_OAUTH_DATABASE_SERVICE_ROLE_KEY: &str = "EYE_DATABASE_SERVICE_ROLE_KEY";
pub const ENV_OAUTH_CALLBACK_URLS: &str = "EYE_OAUTH_CALLBACK_URLS";
pub const ENV_OAUTH_ALLOW_LOCALHOST_HTTP: &str = "EYE_OAUTH_ALLOW_LOCALHOST_HTTP";
pub const ENV_OAUTH_EXCHANGE_SECRET: &str = "EYE_OAUTH_EXCHANGE_SECRET";
pub const ENV_JWT_SIGNING_SECRET: &str = "EYE_JWT_SIGNING_SECRET";
pub const ENV_JWT_KID: &str = "EYE_JWT_KID";

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

        let oauth = parse_oauth_config(&lookup)?;

        Ok(Self {
            bind_addr,
            log_filter,
            vision_provider_enabled,
            vision_provider_api_key,
            image_limits,
            vision,
            oauth,
        })
    }
}

fn parse_oauth_config<F>(lookup: &F) -> Result<Option<OAuthRuntimeConfig>, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    let client_id = non_empty(lookup(ENV_OAUTH_GOOGLE_CLIENT_ID));
    let client_secret = non_empty(lookup(ENV_OAUTH_GOOGLE_CLIENT_SECRET));
    let google_redirect_url = non_empty(lookup(ENV_OAUTH_GOOGLE_REDIRECT_URL));
    let database_url = non_empty(lookup(ENV_OAUTH_DATABASE_REST_URL));
    let database_key = non_empty(lookup(ENV_OAUTH_DATABASE_SERVICE_ROLE_KEY));
    let callback_urls = non_empty(lookup(ENV_OAUTH_CALLBACK_URLS));
    let exchange_secret = non_empty(lookup(ENV_OAUTH_EXCHANGE_SECRET));
    let jwt_secret = non_empty(lookup(ENV_JWT_SIGNING_SECRET));
    let allow_localhost_http = match non_empty(lookup(ENV_OAUTH_ALLOW_LOCALHOST_HTTP)).as_deref() {
        None | Some("false") | Some("0") => false,
        Some("true") | Some("1") => true,
        Some(_) => {
            return Err(ConfigError::Invalid {
                name: ENV_OAUTH_ALLOW_LOCALHOST_HTTP,
                reason: "expected true/false or 1/0".to_owned(),
            });
        }
    };

    let provided = [
        client_id.is_some(),
        client_secret.is_some(),
        google_redirect_url.is_some(),
        database_url.is_some(),
        database_key.is_some(),
        callback_urls.is_some(),
        exchange_secret.is_some(),
        jwt_secret.is_some(),
    ];
    if provided.iter().all(|present| !present) {
        if allow_localhost_http {
            return Err(ConfigError::Invalid {
                name: ENV_OAUTH_ALLOW_LOCALHOST_HTTP,
                reason: "cannot be enabled when OAuth is disabled".to_owned(),
            });
        }
        return Ok(None);
    }
    if provided.iter().any(|present| !present) {
        return Err(ConfigError::Invalid {
            name: ENV_OAUTH_CALLBACK_URLS,
            reason: "OAuth requires all server-side client, database, callback, exchange and JWT settings".to_owned(),
        });
    }

    let client_id = client_id.unwrap_or_default();
    let client_secret = client_secret.unwrap_or_default();
    let google_redirect_url = google_redirect_url.unwrap_or_default();
    let database_url = database_url.unwrap_or_default();
    let database_key = database_key.unwrap_or_default();
    let callback_urls = callback_urls.unwrap_or_default();
    let exchange_secret = exchange_secret.unwrap_or_default();
    let jwt_secret = jwt_secret.unwrap_or_default();

    if exchange_secret.len() < 32 {
        return Err(ConfigError::Invalid {
            name: ENV_OAUTH_EXCHANGE_SECRET,
            reason: "must contain at least 32 bytes".to_owned(),
        });
    }
    if jwt_secret.len() < 32 {
        return Err(ConfigError::Invalid {
            name: ENV_JWT_SIGNING_SECRET,
            reason: "must contain at least 32 bytes".to_owned(),
        });
    }

    let parsed_google_redirect =
        reqwest::Url::parse(&google_redirect_url).map_err(|_| ConfigError::Invalid {
            name: ENV_OAUTH_GOOGLE_REDIRECT_URL,
            reason: "expected an absolute HTTPS URL ending in /v1/auth/google/callback".to_owned(),
        })?;
    if parsed_google_redirect.path() != "/v1/auth/google/callback"
        || !parsed_google_redirect.username().is_empty()
        || parsed_google_redirect.password().is_some()
        || parsed_google_redirect.query().is_some()
        || parsed_google_redirect.fragment().is_some()
        || parsed_google_redirect.as_str() != google_redirect_url
    {
        return Err(ConfigError::Invalid {
            name: ENV_OAUTH_GOOGLE_REDIRECT_URL,
            reason: "must be an exact absolute URL ending in /v1/auth/google/callback".to_owned(),
        });
    }
    match parsed_google_redirect.scheme() {
        "https" => {}
        "http" if allow_localhost_http && is_loopback_host(parsed_google_redirect.host_str()) => {}
        _ => {
            return Err(ConfigError::Invalid {
                name: ENV_OAUTH_GOOGLE_REDIRECT_URL,
                reason: "HTTPS is required except for explicitly enabled localhost development"
                    .to_owned(),
            });
        }
    }

    let parsed_database_url =
        reqwest::Url::parse(&database_url).map_err(|_| ConfigError::Invalid {
            name: ENV_OAUTH_DATABASE_REST_URL,
            reason: "expected an absolute HTTPS PostgREST URL".to_owned(),
        })?;
    if !parsed_database_url.username().is_empty()
        || parsed_database_url.password().is_some()
        || parsed_database_url.query().is_some()
        || parsed_database_url.fragment().is_some()
    {
        return Err(ConfigError::Invalid {
            name: ENV_OAUTH_DATABASE_REST_URL,
            reason: "credentials, query strings and fragments are not allowed in the URL"
                .to_owned(),
        });
    }
    match parsed_database_url.scheme() {
        "https" => {}
        "http" if allow_localhost_http && is_loopback_host(parsed_database_url.host_str()) => {}
        _ => {
            return Err(ConfigError::Invalid {
                name: ENV_OAUTH_DATABASE_REST_URL,
                reason: "HTTPS is required except for explicitly enabled localhost development"
                    .to_owned(),
            });
        }
    }

    let mut seen_callbacks = std::collections::HashSet::new();
    let mut callback_count = 0usize;
    for raw in callback_urls.split(',').map(str::trim) {
        if raw.is_empty() || raw.contains('*') {
            return Err(ConfigError::Invalid {
                name: ENV_OAUTH_CALLBACK_URLS,
                reason: "must be a comma-separated list of exact absolute callback URLs".to_owned(),
            });
        }
        let url = reqwest::Url::parse(raw).map_err(|_| ConfigError::Invalid {
            name: ENV_OAUTH_CALLBACK_URLS,
            reason: "must be a comma-separated list of exact absolute callback URLs".to_owned(),
        })?;
        if url.as_str() != raw
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(ConfigError::Invalid {
                name: ENV_OAUTH_CALLBACK_URLS,
                reason: "callback URLs cannot contain credentials, query strings, fragments or non-canonical forms".to_owned(),
            });
        }
        match url.scheme() {
            "https" => {}
            "http" if allow_localhost_http && is_loopback_host(url.host_str()) => {}
            _ => {
                return Err(ConfigError::Invalid {
                    name: ENV_OAUTH_CALLBACK_URLS,
                    reason: "HTTPS is required except for explicitly enabled localhost development"
                        .to_owned(),
                });
            }
        }
        if !seen_callbacks.insert(raw.to_owned()) {
            continue;
        }
        callback_count += 1;
    }
    if callback_count == 0 {
        return Err(ConfigError::Invalid {
            name: ENV_OAUTH_CALLBACK_URLS,
            reason: "at least one exact callback URL is required".to_owned(),
        });
    }

    Ok(Some(OAuthRuntimeConfig {
        google_client_id: client_id,
        google_redirect_url,
        google_client_secret: SecretString::new(client_secret),
        database_rest_url: database_url.trim_end_matches('/').to_owned(),
        database_service_role_key: SecretString::new(database_key),
        callback_urls,
        allow_localhost_http,
        exchange_secret: SecretString::new(exchange_secret),
        jwt_signing_secret: SecretString::new(jwt_secret),
        jwt_kid: non_empty(lookup(ENV_JWT_KID)).unwrap_or_else(|| "key-2026-v1".to_owned()),
    }))
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
