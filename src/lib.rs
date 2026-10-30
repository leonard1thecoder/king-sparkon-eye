//! King Sparkon Eye: Rust vision service.
//!
//! Rust vision service with bounded multipart ingress, YuNet face detection and
//! conservative photo assessment. Uncalibrated nominal passes are reported as
//! ASSESSMENT_UNAVAILABLE. Enrollment persistence and admission remain separate.

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
