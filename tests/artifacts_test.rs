//! Integrity checks for the vendored inference artifacts. Nothing is downloaded at test time.

use std::path::Path;

use serde_json::Value;
use sha2::{Digest, Sha256};

fn manifest() -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let text =
        std::fs::read_to_string(root.join("artifacts/manifest.json")).expect("manifest exists");
    serde_json::from_str(&text).expect("manifest is valid JSON")
}

fn sha256_hex(path: &Path) -> (String, u64) {
    let bytes =
        std::fs::read(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let digest = Sha256::digest(&bytes);
    (
        digest.iter().map(|b| format!("{b:02x}")).collect(),
        bytes.len() as u64,
    )
}

#[test]
fn every_artifact_matches_its_recorded_hash_and_size() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let manifest = manifest();
    let artifacts = manifest["artifacts"].as_array().expect("artifacts array");
    assert!(!artifacts.is_empty());
    for artifact in artifacts {
        let file = root.join(artifact["file"].as_str().unwrap());
        let (hash, size) = sha256_hex(&file);
        assert_eq!(
            hash,
            artifact["sha256"].as_str().unwrap(),
            "hash mismatch for {}",
            file.display()
        );
        assert_eq!(
            size,
            artifact["size_bytes"].as_u64().unwrap(),
            "size mismatch for {}",
            file.display()
        );
    }
}

#[test]
fn every_artifact_has_a_matching_license_file() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for artifact in manifest()["artifacts"].as_array().unwrap() {
        let license = root.join(artifact["license_file"].as_str().unwrap());
        let (hash, _) = sha256_hex(&license);
        assert_eq!(
            hash,
            artifact["license_sha256"].as_str().unwrap(),
            "license mismatch for {}",
            license.display()
        );
        assert!(
            artifact["license"].as_str().unwrap().starts_with("MIT"),
            "licence must be recorded and permissive"
        );
    }
}

#[test]
fn model_io_is_documented_and_matches_the_verified_shapes() {
    let manifest = manifest();
    let model = manifest["artifacts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["id"] == "yunet-2023mar")
        .expect("model entry");
    assert_eq!(
        model["io"]["input"]["shape"],
        serde_json::json!([1, 3, 640, 640])
    );
    let outputs = model["io"]["outputs"].as_array().unwrap();
    assert_eq!(outputs.len(), 4, "cls, obj, bbox and kps groups");
}
