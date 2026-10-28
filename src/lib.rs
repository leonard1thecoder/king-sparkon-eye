//! King Sparkon Eye: Rust vision service.
//!
//! Current scope (issue #21): crate skeleton, configuration, HTTP liveness and
//! readiness endpoints, structured logging and graceful shutdown. Image
//! validation, ONNX inference, provider adapters and assessment policy are
//! planned in later issues and are intentionally not implemented here.

pub mod api;
pub mod config;
pub mod contract;
pub mod domain;
pub mod error;
pub mod inference;
pub mod observability;
pub mod provider;
