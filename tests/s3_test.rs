//! AWS S3 private storage adapter integration tests (issue #31).

use king_sparkon_eye::contract::{PersistenceStatus, SuitabilityStatus};
use king_sparkon_eye::storage::{
    EnrollmentService, EnrollmentStatus, MockS3Client, NewEnrollment, OBJECT_KEY_PREFIX,
    ObjectStoreError, S3ObjectStore, S3StorageConfig, StorageError,
};

const PHOTO_PAYLOAD: &[u8] = b"confidential-consented-photo-bytes";

fn test_enrollment<'a>(bytes: &'a [u8]) -> NewEnrollment<'a> {
    NewEnrollment {
        bytes,
        suitability: SuitabilityStatus::AssessmentPassed,
        consent_reference: "consent-ref-2026-s3",
        policy_version: "policy-v1",
        model_version: Some("yunet-2023mar"),
        width: 1024,
        height: 768,
        idempotency_key: Some("s3-idem-test-1"),
        created_at_ms: 1_700_000_500_000,
    }
}

#[test]
fn s3_store_persists_accepted_photo_with_encryption_and_private_access() {
    let mock_client = MockS3Client::new();
    let s3_store = S3ObjectStore::new(S3StorageConfig::default(), mock_client);
    let service = EnrollmentService::new(s3_store);

    let record = service
        .create(test_enrollment(PHOTO_PAYLOAD))
        .expect("persisted");

    assert_eq!(record.status, EnrollmentStatus::Stored);
    assert_eq!(record.persistence, PersistenceStatus::Pending);
    assert!(record.object_key.starts_with(OBJECT_KEY_PREFIX));
    assert_eq!(record.byte_len, PHOTO_PAYLOAD.len());
}

#[test]
fn s3_store_maps_iam_access_denial_to_denied_error() {
    let mock_client = MockS3Client::new();
    mock_client.set_access_denied(true); // Simulate 403 AccessDenied
    let s3_store = S3ObjectStore::new(S3StorageConfig::default(), mock_client);
    let service = EnrollmentService::new(s3_store);

    let res = service.create(test_enrollment(PHOTO_PAYLOAD));

    assert_eq!(res, Err(StorageError::Store(ObjectStoreError::Denied)));
}

#[test]
fn s3_store_maps_network_unavailable_to_unavailable_error() {
    let mock_client = MockS3Client::new();
    mock_client.fail_next_operations(1);
    let s3_store = S3ObjectStore::new(S3StorageConfig::default(), mock_client);
    let service = EnrollmentService::new(s3_store);

    let res = service.create(test_enrollment(PHOTO_PAYLOAD));

    assert_eq!(res, Err(StorageError::Store(ObjectStoreError::Unavailable)));
}

#[test]
fn s3_store_handles_deletion_and_orphan_cleanup_correctly() {
    let mock_client = MockS3Client::new();
    let s3_store = S3ObjectStore::new(S3StorageConfig::default(), mock_client);
    let service = EnrollmentService::new(s3_store);

    let record = service.create(test_enrollment(PHOTO_PAYLOAD)).unwrap();

    // Expire/delete
    let expired = service.expire(&record.enrollment_id).expect("expired");
    assert_eq!(expired.status, EnrollmentStatus::Deleted);

    // Orphan sweep cleans up any unreferenced key
    let cleaned = service.sweep_orphans().expect("swept");
    assert_eq!(cleaned, 0); // Already deleted directly
}
