//! Shared ONNX Runtime environment for all inference adapters.
//!
//! ONNX Runtime is initialized once per process. Detector and recognizer sessions must use
//! the same dynamic-library path; a second path is rejected instead of creating a split runtime.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use ort::environment::Environment;

use super::InferenceError;

/// The first configured runtime path wins for the lifetime of the process.
static ENVIRONMENT: OnceLock<(PathBuf, Result<Environment, InferenceError>)> = OnceLock::new();

pub(crate) fn environment(runtime_path: &Path) -> Result<&'static Environment, InferenceError> {
    let (loaded_path, env) = ENVIRONMENT.get_or_init(|| {
        let env = ort::init_from(runtime_path)
            .map_err(|_| InferenceError::Runtime)
            .and_then(|builder| {
                builder
                    .with_name("king-sparkon-eye")
                    .build()
                    .map_err(|_| InferenceError::Runtime)
            });
        (runtime_path.to_path_buf(), env)
    });
    if loaded_path != runtime_path {
        return Err(InferenceError::Runtime);
    }
    env.as_ref().map_err(|error| *error)
}
