//! Meta Muse Spark adapter (issue #25).
//!
//! Verified against Meta's API reference (https://dev.meta.ai/docs):
//! - base URL `https://api.meta.ai/v1`, `POST /chat/completions`, `Authorization: Bearer <key>`;
//! - model `muse-spark-1.3` (the standard variant. Its inputs are not used to improve Meta's products);
//! - images passed as `image_url` parts with base64 data URLs;
//! - `response_format` with `json_schema` is supported;
//! - errors use an `error` object. Retries are documented for 429, 500 and 503 (with `Retry-After`,
//!   when present). 504 must not be simply retried, and 400, 401, 402, 403, 404 and 413 must not be retried.
//!
//! Not documented by Meta, and not assumed here: log or data-retention periods for API inputs,
//! the image dimension limit, and a 502 retry policy. See README.

use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};

use crate::advisory::{SYSTEM_PROMPT, USER_PROMPT, response_schema};
use crate::config::SecretString;
use crate::provider::{ProviderError, VisionProvider};

pub const DEFAULT_BASE_URL: &str = "https://api.meta.ai/v1";
pub const DEFAULT_MODEL: &str = "muse-spark-1.3";
/// Upper bound on retries, whatever the configuration says.
pub const MAX_RETRIES_CAP: u32 = 5;
const MAX_BACKOFF: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct MuseSettings {
    pub base_url: String,
    pub model: String,
    pub api_key: SecretString,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub max_retries: u32,
    /// Base for exponential backoff. Tests set this to zero.
    pub retry_base: Duration,
}

/// Internal failure with retry guidance.
#[derive(Debug, Clone, Copy)]
struct Attempt {
    error: ProviderError,
    retryable: bool,
    retry_after: Option<Duration>,
}

impl Attempt {
    fn fatal(error: ProviderError) -> Self {
        Self {
            error,
            retryable: false,
            retry_after: None,
        }
    }
}

#[derive(Debug)]
pub struct MuseSparkProvider {
    client: reqwest::Client,
    settings: MuseSettings,
}

impl MuseSparkProvider {
    pub fn new(settings: MuseSettings) -> Result<Self, ProviderError> {
        if settings.api_key.expose().is_empty() {
            return Err(ProviderError::MissingCredentials);
        }
        let client = reqwest::Client::builder()
            .timeout(settings.timeout)
            .build()
            .map_err(|_| ProviderError::Unavailable)?;
        Ok(Self { client, settings })
    }

    /// Builds the request body. The image goes in as a PNG data URL, and the schema constrains the answer.
    pub fn request_body(&self, png: &[u8]) -> Value {
        let data_url = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(png)
        );
        json!({
            "model": self.settings.model,
            "temperature": 0,
            "max_completion_tokens": 512,
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": "photo_guidance", "strict": true, "schema": response_schema()}
            },
            "messages": [
                {"role": "system", "content": SYSTEM_PROMPT},
                {"role": "user", "content": [
                    {"type": "text", "text": USER_PROMPT},
                    {"type": "image_url", "image_url": {"url": data_url}}
                ]}
            ]
        })
    }

    async fn send_once(&self, body: &Value) -> Result<String, Attempt> {
        let url = format!(
            "{}/chat/completions",
            self.settings.base_url.trim_end_matches('/')
        );
        let response = self
            .client
            .post(url)
            .bearer_auth(self.settings.api_key.expose())
            .json(body)
            .send()
            .await
            .map_err(|e| Attempt::fatal(map_transport(&e)))?;

        let status = response.status();
        if !status.is_success() {
            let retry_after = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            return Err(map_status(status.as_u16(), retry_after));
        }

        let bytes = read_capped(response, self.settings.max_response_bytes)
            .await
            .map_err(Attempt::fatal)?;
        extract_content(&bytes).map_err(Attempt::fatal)
    }

    async fn run(&self, png: Vec<u8>) -> Result<String, ProviderError> {
        let body = self.request_body(&png);
        drop(png);
        let attempts = self.settings.max_retries.min(MAX_RETRIES_CAP) + 1;
        let mut last = ProviderError::Unavailable;
        for attempt in 0..attempts {
            match self.send_once(&body).await {
                Ok(text) => return Ok(text),
                Err(failure) => {
                    last = failure.error;
                    if !failure.retryable || attempt + 1 == attempts {
                        return Err(failure.error);
                    }
                    // Only the error kind is logged. Never the body or the image.
                    tracing::warn!(attempt, error = ?failure.error, "vision provider attempt failed; retrying");
                    tokio::time::sleep(backoff(
                        self.settings.retry_base,
                        attempt,
                        failure.retry_after,
                    ))
                    .await;
                }
            }
        }
        Err(last)
    }
}

impl VisionProvider for MuseSparkProvider {
    fn advise(
        &self,
        png: Vec<u8>,
    ) -> impl std::future::Future<Output = Result<String, ProviderError>> + Send {
        self.run(png)
    }
}

/// Exponential backoff with jitter, capped. A `Retry-After` header takes priority, also capped.
fn backoff(base: Duration, attempt: u32, retry_after: Option<Duration>) -> Duration {
    if let Some(ra) = retry_after {
        return ra.min(MAX_BACKOFF);
    }
    let exp = base.saturating_mul(2u32.saturating_pow(attempt));
    let nanos = u64::from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0),
    );
    let jitter = base.mul_f64((nanos % 1000) as f64 / 1000.0);
    (exp + jitter).min(MAX_BACKOFF)
}

fn map_transport(err: &reqwest::Error) -> ProviderError {
    if err.is_timeout() {
        ProviderError::Timeout
    } else {
        ProviderError::Unavailable
    }
}

/// Maps HTTP status to a provider error and to whether a retry is documented.
fn map_status(status: u16, retry_after: Option<Duration>) -> Attempt {
    match status {
        429 => Attempt {
            error: ProviderError::RateLimited,
            retryable: true,
            retry_after,
        },
        500 | 503 => Attempt {
            error: ProviderError::Unavailable,
            retryable: true,
            retry_after,
        },
        504 => Attempt::fatal(ProviderError::Timeout),
        500..=599 => Attempt::fatal(ProviderError::Unavailable),
        _ => Attempt::fatal(ProviderError::Rejected),
    }
}

/// Reads the body up to `max` bytes, aborting as soon as it is exceeded.
async fn read_capped(
    mut response: reqwest::Response,
    max: usize,
) -> Result<Vec<u8>, ProviderError> {
    let mut buf = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if buf.len() + chunk.len() > max {
                    return Err(ProviderError::ResponseTooLarge);
                }
                buf.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(buf),
            Err(e) => return Err(map_transport(&e)),
        }
    }
}

/// Pulls the assistant text out of a chat completion, and checks that it finished cleanly.
pub fn extract_content(bytes: &[u8]) -> Result<String, ProviderError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| ProviderError::InvalidResponse)?;
    let choice = value
        .get("choices")
        .and_then(|c| c.get(0))
        .ok_or(ProviderError::InvalidResponse)?;
    if choice.get("finish_reason").and_then(Value::as_str) != Some("stop") {
        return Err(ProviderError::InvalidResponse);
    }
    let message = choice
        .get("message")
        .ok_or(ProviderError::InvalidResponse)?;
    if message.get("refusal").is_some_and(|r| !r.is_null()) {
        return Err(ProviderError::InvalidResponse);
    }
    message
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(ProviderError::InvalidResponse)
}
