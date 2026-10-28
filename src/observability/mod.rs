//! Structured logging setup. Initialisation is idempotent so tests can call it safely.

use tracing_subscriber::{EnvFilter, fmt};

/// Installs a JSON-formatted tracing subscriber using the given filter directive.
/// Falls back to `info` if the directive is invalid. Safe to call more than once.
pub fn init_tracing(filter: &str) {
    let env_filter = EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt()
        .with_env_filter(env_filter)
        .json()
        .with_current_span(false)
        .try_init();
}
