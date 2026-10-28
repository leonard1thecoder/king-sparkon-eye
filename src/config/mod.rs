//! Environment-based configuration.
//!
//! Secrets are wrapped in [`SecretString`], whose `Debug` output is redacted,
//! and configuration errors name the variable but never echo its value.

use std::fmt;
use std::net::SocketAddr;

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

/// Runtime configuration for the service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub bind_addr: SocketAddr,
    pub log_filter: String,
    /// Whether the optional frontier vision provider (issue #25) is enabled.
    pub vision_provider_enabled: bool,
    /// Credential for the vision provider. Optional; required only when the provider is enabled.
    pub vision_provider_api_key: Option<SecretString>,
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

        Ok(Self {
            bind_addr,
            log_filter,
            vision_provider_enabled,
            vision_provider_api_key,
        })
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}
