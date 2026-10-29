//! King Sparkon Eye: Rust vision service.
//!
//! Current scope (issue #21): crate skeleton, configuration, HTTP liveness and
//! readiness endpoints, structured logging and graceful shutdown. Image
//! validation, ONNX inference, provider adapters and assessment policy are
//! planned in later issues and are intentionally not implemented here.

pub mod advisory;
pub mod api;
pub mod auth;
pub mod config;
pub mod contract;
pub mod domain;

// Enrollment/outbox draft modules stay unexported until their dependency, runtime wiring,
// and private backend consent-contract gates are satisfied (issues #28 and #113).
pub mod error;
pub mod inference;
pub mod ingress;
pub mod observability;
pub mod provider;
pub mod quality;
pub mod storage;
