//! Versioned Kafka enrollment event tests (issue #28).

use king_sparkon_eye::contract::PersistenceStatus;
use king_sparkon_eye::domain::events::{
    ENROLLMENT_EVENT_TYPE, ENROLLMENT_EVENT_VERSION, ENROLLMENT_EVENTS_TOPIC,
    EnrollmentEventPublisher, KafkaPublishError, MemoryKafkaProducer, PhotoEnrolledEventV1,
};
use king_sparkon_eye::storage::{EnrollmentRecord, EnrollmentStatus};

fn sample_record() -> EnrollmentRecord {
    EnrollmentRecord {
        enrollment_id: "enr-001".to_string(),
        object_key: "enrollments/random-uuid-key".to_string(),
        content_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
            .to_string(),
        byte_len: 2048,
        width: 640,
        height: 480,
        policy_version: "policy-v1".to_string(),
        model_version: Some("yunet-2023mar".to_string()),
        consent_reference: "consent-ref-123".to_string(),
        idempotency_key: Some("idem-key-abc".to_string()),
        status: EnrollmentStatus::Stored,
        persistence: PersistenceStatus::Pending,
        created_at_ms: 1_700_000_000_000,
    }
}

#[test]
fn event_is_constructed_correctly_from_enrollment_record() {
    let record = sample_record();
    let event =
        PhotoEnrolledEventV1::from_record(&record, "evt-999".to_string(), 1_700_000_001_000);

    assert_eq!(event.event_id, "evt-999");
    assert_eq!(event.event_type, ENROLLMENT_EVENT_TYPE);
    assert_eq!(event.event_version, ENROLLMENT_EVENT_VERSION);
    assert_eq!(event.enrollment_id, "enr-001");
    assert_eq!(event.object_key, "enrollments/random-uuid-key");
    assert_eq!(event.content_sha256, record.content_sha256);
    assert_eq!(event.dimensions.width, 640);
    assert_eq!(event.dimensions.height, 480);
    assert_eq!(event.consent_reference, "consent-ref-123");
    assert_eq!(event.idempotency_key.as_deref(), Some("idem-key-abc"));
    assert_eq!(event.persistence, PersistenceStatus::Pending);
}

#[test]
fn event_payload_never_contains_raw_photo_or_embedding() {
    let record = sample_record();
    let event =
        PhotoEnrolledEventV1::from_record(&record, "evt-999".to_string(), 1_700_000_001_000);
    let json = serde_json::to_string(&event).expect("serialized");

    // Guard against biometric or raw payload leakage into Kafka
    assert!(!json.contains("bytes"));
    assert!(!json.contains("raw"));
    assert!(!json.contains("embedding"));
    assert!(!json.contains("landmark"));
    assert!(!json.contains("template"));
}

#[test]
fn successful_publish_records_message_in_memory_producer() {
    let memory_producer = MemoryKafkaProducer::new();
    let publisher = EnrollmentEventPublisher::new(memory_producer);

    let record = sample_record();
    let event =
        PhotoEnrolledEventV1::from_record(&record, "evt-001".to_string(), 1_700_000_000_000);

    let res = publisher.publish_enrollment_event(&event);
    assert_eq!(res.unwrap(), "evt-001");

    let messages = publisher.producer().published_messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].topic, ENROLLMENT_EVENTS_TOPIC);
    assert_eq!(messages[0].key, "idem-key-abc");
    assert!(messages[0].payload_json.contains("\"eventId\":\"evt-001\""));
    assert!(
        messages[0]
            .payload_json
            .contains("\"eventType\":\"photo.enrolled.v1\"")
    );
}

#[test]
fn idempotent_republish_returns_previous_event_id_without_duplicate_message() {
    let memory_producer = MemoryKafkaProducer::new();
    let publisher = EnrollmentEventPublisher::new(memory_producer);

    let record = sample_record();
    let event1 =
        PhotoEnrolledEventV1::from_record(&record, "evt-001".to_string(), 1_700_000_000_000);
    let event2 =
        PhotoEnrolledEventV1::from_record(&record, "evt-002".to_string(), 1_700_000_000_500);

    let res1 = publisher.publish_enrollment_event(&event1).unwrap();
    let res2 = publisher.publish_enrollment_event(&event2).unwrap();

    assert_eq!(res1, "evt-001");
    assert_eq!(res2, "evt-001"); // Returns existing event_id

    // Only one message actually sent to broker
    let messages = publisher.producer().published_messages();
    assert_eq!(messages.len(), 1);
}

#[test]
fn publisher_retries_and_recovers_if_initial_attempt_fails() {
    let memory_producer = MemoryKafkaProducer::new();
    memory_producer.fail_next_publishes(2); // Fail first 2 attempts, 3rd succeeds

    let publisher = EnrollmentEventPublisher::new(memory_producer);
    let record = sample_record();
    let event =
        PhotoEnrolledEventV1::from_record(&record, "evt-retry".to_string(), 1_700_000_000_000);

    let res = publisher.publish_enrollment_event(&event);
    assert!(res.is_ok());

    let messages = publisher.producer().published_messages();
    assert_eq!(messages.len(), 1);
}

#[test]
fn publisher_fails_if_retries_exhausted_and_does_not_corrupt_state() {
    let memory_producer = MemoryKafkaProducer::new();
    memory_producer.fail_next_publishes(5); // More than max 3 attempts

    let publisher = EnrollmentEventPublisher::new(memory_producer);
    let record = sample_record();
    let event =
        PhotoEnrolledEventV1::from_record(&record, "evt-fail".to_string(), 1_700_000_000_000);

    let res = publisher.publish_enrollment_event(&event);
    assert_eq!(res, Err(KafkaPublishError::Unavailable));

    let messages = publisher.producer().published_messages();
    assert_eq!(messages.len(), 0);
}

#[test]
fn invalid_identifiers_in_event_are_rejected_before_publish() {
    let memory_producer = MemoryKafkaProducer::new();
    let publisher = EnrollmentEventPublisher::new(memory_producer);

    let mut record = sample_record();
    record.enrollment_id = "invalid id with spaces!".to_string();

    let event =
        PhotoEnrolledEventV1::from_record(&record, "evt-001".to_string(), 1_700_000_000_000);
    let res = publisher.publish_enrollment_event(&event);

    assert!(matches!(res, Err(KafkaPublishError::ValidationError(_))));
    assert_eq!(publisher.producer().published_messages().len(), 0);
}
