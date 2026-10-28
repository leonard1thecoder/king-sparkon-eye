//! Private storage and enrollment boundary tests (issue #27). In-memory store only.

use king_sparkon_eye::contract::{PersistenceStatus, SuitabilityStatus};
use king_sparkon_eye::storage::{
    EnrollmentService, EnrollmentStatus, MemoryObjectStore, NewEnrollment, OBJECT_KEY_PREFIX,
    ObjectStore, StorageError,
};

const PHOTO: &[u8] = b"synthetic-original-bytes-not-a-face";

fn input<'a>(bytes: &'a [u8]) -> NewEnrollment<'a> {
    NewEnrollment {
        bytes,
        suitability: SuitabilityStatus::AssessmentPassed,
        consent_reference: "consent-2026-001",
        policy_version: "policy-v1",
        model_version: Some("yunet-2023mar"),
        width: 640,
        height: 480,
        idempotency_key: None,
        created_at_ms: 1_700_000_000_000,
    }
}

fn service() -> EnrollmentService<MemoryObjectStore> {
    EnrollmentService::new(MemoryObjectStore::new())
}

// ---------- creating records ----------

#[test]
fn passing_consented_photo_is_stored_privately_with_pending_persistence() {
    let svc = service();
    let record = svc.create(input(PHOTO)).expect("stored");

    assert_eq!(record.status, EnrollmentStatus::Stored);
    assert_eq!(record.persistence, PersistenceStatus::Pending);
    assert_eq!(record.byte_len, PHOTO.len());
    assert_eq!(svc.store().get(&record.object_key).as_deref(), Some(PHOTO));
    assert!(record.object_key.starts_with(OBJECT_KEY_PREFIX));
}

#[test]
fn object_keys_are_random_and_do_not_reveal_the_content() {
    let svc = service();
    let a = svc.create(input(PHOTO)).unwrap();
    let b = svc.create(input(PHOTO)).unwrap();
    assert_ne!(a.object_key, b.object_key);
    assert!(!a.object_key.contains("consent"));
    assert!(!a.object_key.contains(&a.content_sha256));
}

#[test]
fn content_hash_is_sha256_of_the_stored_bytes() {
    let svc = service();
    let record = svc.create(input(b"abc")).unwrap();
    // Known SHA-256 of "abc".
    assert_eq!(
        record.content_sha256,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

#[test]
fn every_suitability_other_than_passed_is_refused_with_no_record_or_object() {
    for suitability in [
        SuitabilityStatus::RetakeRequired,
        SuitabilityStatus::ReviewRequired,
        SuitabilityStatus::AssessmentUnavailable,
    ] {
        let svc = service();
        let mut i = input(PHOTO);
        i.suitability = suitability;
        assert_eq!(
            svc.create(i),
            Err(StorageError::NotSuitable),
            "{suitability:?}"
        );
        assert!(svc.store().is_empty(), "no object for {suitability:?}");
    }
}

#[test]
fn invalid_consent_reference_is_refused_before_any_write() {
    let svc = service();
    let mut i = input(PHOTO);
    i.consent_reference = "";
    assert!(matches!(svc.create(i), Err(StorageError::Validation(_))));

    let mut i = input(PHOTO);
    i.consent_reference = "bad/ref with spaces";
    assert!(matches!(svc.create(i), Err(StorageError::Validation(_))));
    assert!(svc.store().is_empty());
}

// ---------- storage failures never produce a false success ----------

#[test]
fn storage_failure_returns_a_typed_error_and_no_record() {
    let svc = service();
    svc.store().fail_next_puts(1);
    let err = svc.create(input(PHOTO)).unwrap_err();
    assert!(matches!(err, StorageError::Store(_)));
    assert!(svc.store().is_empty());
    assert!(svc.sweep_orphans().unwrap() == 0);
}

#[test]
fn a_retry_after_a_storage_failure_succeeds() {
    let svc = service();
    svc.store().fail_next_puts(1);
    assert!(svc.create(input(PHOTO)).is_err());
    let record = svc.create(input(PHOTO)).expect("retry stored");
    assert_eq!(record.status, EnrollmentStatus::Stored);
}

#[test]
fn error_text_never_reveals_keys_or_bytes() {
    let svc = service();
    svc.store().fail_next_puts(1);
    let err = svc.create(input(PHOTO)).unwrap_err();
    let text = format!("{err} {err:?}");
    assert!(!text.contains(OBJECT_KEY_PREFIX));
    assert!(!text.contains("synthetic"));
}

// ---------- idempotency ----------

#[test]
fn same_key_and_same_content_returns_the_existing_record() {
    let svc = service();
    let mut i = input(PHOTO);
    i.idempotency_key = Some("req-123");
    let first = svc.create(i).unwrap();
    let second = svc.create(i).unwrap();
    assert_eq!(first, second);
    assert_eq!(svc.store().len(), 1, "no second object");
}

#[test]
fn same_key_with_different_bytes_is_a_conflict() {
    let svc = service();
    let mut i = input(PHOTO);
    i.idempotency_key = Some("req-123");
    svc.create(i).unwrap();

    let other = b"different-bytes";
    let mut j = input(other);
    j.idempotency_key = Some("req-123");
    assert_eq!(svc.create(j), Err(StorageError::IdempotencyConflict));
    assert_eq!(svc.store().len(), 1);
}

#[test]
fn same_key_with_a_different_consent_reference_is_a_conflict() {
    let svc = service();
    let mut i = input(PHOTO);
    i.idempotency_key = Some("req-9");
    svc.create(i).unwrap();

    let mut j = input(PHOTO);
    j.idempotency_key = Some("req-9");
    j.consent_reference = "consent-2026-999";
    assert_eq!(svc.create(j), Err(StorageError::IdempotencyConflict));
}

// ---------- expiry and cleanup ----------

#[test]
fn expiry_deletes_the_object_and_keeps_an_audit_record() {
    let svc = service();
    let record = svc.create(input(PHOTO)).unwrap();
    let expired = svc.expire(&record.enrollment_id).unwrap();

    assert_eq!(expired.status, EnrollmentStatus::Deleted);
    assert!(svc.store().get(&record.object_key).is_none());
    assert_eq!(
        svc.get(&record.enrollment_id).unwrap().status,
        EnrollmentStatus::Deleted
    );
}

#[test]
fn expiry_is_idempotent() {
    let svc = service();
    let record = svc.create(input(PHOTO)).unwrap();
    svc.expire(&record.enrollment_id).unwrap();
    assert!(svc.expire(&record.enrollment_id).is_ok());
}

#[test]
fn expiring_an_unknown_enrollment_is_not_found() {
    let svc = service();
    assert_eq!(svc.expire("missing"), Err(StorageError::NotFound));
}

#[test]
fn orphaned_objects_are_swept_and_stored_ones_are_kept() {
    let svc = service();
    let kept = svc.create(input(PHOTO)).unwrap();

    // Simulates a crash after the object was written but before the record was inserted.
    svc.store()
        .put(&format!("{OBJECT_KEY_PREFIX}orphan-from-crash"), b"lost")
        .unwrap();
    // A key outside this service's prefix must never be touched.
    svc.store().put("other-service/data", b"not ours").unwrap();

    assert_eq!(svc.sweep_orphans().unwrap(), 1);
    assert!(svc.store().get(&kept.object_key).is_some());
    assert!(svc.store().get("other-service/data").is_some());
    assert!(
        svc.store()
            .get(&format!("{OBJECT_KEY_PREFIX}orphan-from-crash"))
            .is_none()
    );
}

#[test]
fn sweep_removes_objects_of_expired_records_that_were_never_deleted() {
    let svc = service();
    let record = svc.create(input(PHOTO)).unwrap();
    // Expire the record but leave the object behind, as a failed delete would.
    svc.expire(&record.enrollment_id).unwrap();
    svc.store().put(&record.object_key, PHOTO).unwrap();

    assert_eq!(svc.sweep_orphans().unwrap(), 1);
    assert!(svc.store().get(&record.object_key).is_none());
}

#[test]
fn sweep_on_a_clean_store_removes_nothing() {
    let svc = service();
    svc.create(input(PHOTO)).unwrap();
    assert_eq!(svc.sweep_orphans().unwrap(), 0);
}

#[test]
fn get_returns_not_found_for_unknown_ids() {
    let svc = service();
    assert_eq!(svc.get("missing"), Err(StorageError::NotFound));
}
