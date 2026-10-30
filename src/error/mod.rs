//! Typed API errors. Responses carry stable codes and fixed messages. They never
//! include stack traces, internal paths, provider payloads or secrets.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

/// Stable, machine-readable error codes. Wire names are listed in openapi/eye.openapi.json.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    NotReady,
    NotFound,
    ValidationFailed,
    PayloadTooLarge,
    UnsupportedMediaType,
    RateLimited,
    ServiceUnavailable,
    ImageMalformed,
    ImageDimensionsExceeded,
    AssessmentNotImplemented,
    Internal,
}

impl ErrorCode {
    pub const ALL: &'static [ErrorCode] = &[
        ErrorCode::NotReady,
        ErrorCode::NotFound,
        ErrorCode::ValidationFailed,
        ErrorCode::PayloadTooLarge,
        ErrorCode::UnsupportedMediaType,
        ErrorCode::RateLimited,
        ErrorCode::ServiceUnavailable,
        ErrorCode::ImageMalformed,
        ErrorCode::ImageDimensionsExceeded,
        ErrorCode::AssessmentNotImplemented,
        ErrorCode::Internal,
    ];
}

/// Wire body for every error response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

/// A rejected field. Only the field name and rule are exposed, never the submitted value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("field `{field}` {rule}")]
pub struct ValidationError {
    pub field: &'static str,
    pub rule: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    #[error("service is not ready")]
    NotReady,
    #[error("resource not found")]
    NotFound,
    #[error("request validation failed: {0}")]
    Validation(ValidationError),
    #[error("payload exceeds the allowed size")]
    PayloadTooLarge,
    #[error("unsupported media type")]
    UnsupportedMediaType,
    #[error("too many requests")]
    RateLimited,
    #[error("a required dependency is temporarily unavailable")]
    ServiceUnavailable,
    #[error("uploaded image is malformed or truncated")]
    ImageMalformed,
    #[error("image dimensions exceed the allowed limits")]
    ImageDimensionsExceeded,
    #[error("image processing timed out")]
    ImageDecodeTimeout,
    #[error("photo assessment is not available until the assessment pipeline is wired")]
    AssessmentNotImplemented,
    /// Deliberately generic: internal causes are logged server-side, never returned.
    #[error("internal error")]
    Internal,
}

impl ApiError {
    pub fn code(&self) -> ErrorCode {
        match self {
            ApiError::NotReady => ErrorCode::NotReady,
            ApiError::NotFound => ErrorCode::NotFound,
            ApiError::Validation(_) => ErrorCode::ValidationFailed,
            ApiError::PayloadTooLarge => ErrorCode::PayloadTooLarge,
            ApiError::UnsupportedMediaType => ErrorCode::UnsupportedMediaType,
            ApiError::RateLimited => ErrorCode::RateLimited,
            ApiError::ServiceUnavailable | ApiError::ImageDecodeTimeout => {
                ErrorCode::ServiceUnavailable
            }
            ApiError::ImageMalformed => ErrorCode::ImageMalformed,
            ApiError::ImageDimensionsExceeded => ErrorCode::ImageDimensionsExceeded,
            ApiError::AssessmentNotImplemented => ErrorCode::AssessmentNotImplemented,
            ApiError::Internal => ErrorCode::Internal,
        }
    }

    pub fn status(&self) -> StatusCode {
        match self {
            ApiError::NotReady | ApiError::ServiceUnavailable | ApiError::ImageDecodeTimeout => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            ApiError::ImageMalformed => StatusCode::BAD_REQUEST,
            ApiError::ImageDimensionsExceeded => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::AssessmentNotImplemented => StatusCode::NOT_IMPLEMENTED,
            ApiError::NotFound => StatusCode::NOT_FOUND,
            ApiError::Validation(_) => StatusCode::BAD_REQUEST,
            ApiError::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            ApiError::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ApiError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            ApiError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    pub fn body(&self) -> ErrorBody {
        ErrorBody {
            code: self.code(),
            message: self.to_string(),
        }
    }
}

impl From<ValidationError> for ApiError {
    fn from(err: ValidationError) -> Self {
        ApiError::Validation(err)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status(), Json(self.body())).into_response()
    }
}
