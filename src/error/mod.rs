//! Typed API errors. Responses carry stable codes and never include stack traces,
//! internal paths or provider payloads.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    NotReady,
    NotFound,
    Internal,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct ErrorBody {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("service is not ready")]
    NotReady,
    #[error("resource not found")]
    NotFound,
}

impl ApiError {
    pub fn status(&self) -> StatusCode {
        match self {
            ApiError::NotReady => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::NotFound => StatusCode::NOT_FOUND,
        }
    }

    pub fn body(&self) -> ErrorBody {
        match self {
            ApiError::NotReady => ErrorBody {
                code: ErrorCode::NotReady,
                message: self.to_string(),
            },
            ApiError::NotFound => ErrorBody {
                code: ErrorCode::NotFound,
                message: self.to_string(),
            },
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status(), Json(self.body())).into_response()
    }
}
