//! King Sparkon Eye: Rust vision service.
//!
//! Current scope (issue #21): crate skeleton, configuration, HTTP liveness and
//! readiness endpoints, structured logging and graceful shutdown. The photo
//! upload route enforces bounded multipart ingress and normalizes supported
//! images; detector/provider/assessment orchestration is not yet wired to it.

pub mod advisory;
pub mod api;
pub mod auth;
pub mod config;
pub mod contract;
pub mod domain;
pub mod error;
pub mod inference;
pub mod ingress;
pub mod observability;
pub mod persistence;
pub mod provider;
pub mod quality;
pub mod storage;
