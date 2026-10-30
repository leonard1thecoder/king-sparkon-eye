//! PostgreSQL integration tests for Eye's durable enrollment outbox (#113).
//! Requires EYE_TEST_DATABASE_URL pointing at a disposable Eye PostgreSQL database.

use king_sparkon_eye::persistence::{connect, migrate, enrollment_outbox::{
    CommitOutcome, EnrollmentOutboxInput, EnrollmentOutboxStore, OutboxStoreError,
}};
use serde_json::json;
use uuid::Uuid;

async fn store() -> Option<EnrollmentOutboxStore> {
    let url = match std::env::var("EYE_TEST_DATABASE_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; enrollment outbox tests not run");
            return None;
        }
    };
    let pool = connect(&url).await.expect("connect to Eye test PostgreSQL");
    migrate(&pool).await.expect("apply SQLx migrations");
    Some(EnrollmentOutboxStore::new(pool))
}

fn input(_tag: &str) -> EnrollmentOutboxInput {
    let id = Uuid::new_v4().simple().to_string();
    let idem = format!("idem_{}", Uuid::new_v4().simple());
    EnrollmentOutboxInput {
        enrollment_id: format!("enr_{id}"),
        event_id: format!("evt_{id}"),
        consent_reference: format!("consent_{id}"),
        object_key: format!("enrollments/{id}"),
        content_sha256: "a".repeat(64),
        byte_len: 1234,
        image_width: 640,
        image_height: 480,
        policy_version: "policy-v1".into(),
        model_version: Some("model-v1".into()),
        idempotency_key: Some(idem.clone()),
        created_at_ms: 1_800_000_000_000_i64,
        payload: json!({
            "eventId": format!("evt_{id}"),
            "eventType": "photo.enrolled.v1",
            "eventVersion": "1.0.0",
            "enrollmentId": format!("enr_{id}"),
            "objectKey": format!("enrollments/{id}"),
            "contentSha256": "a".repeat(64),
            "byteLen": 1234,
            "dimensions": {"width":640,"height":480},
            "consentReference": format!("consent_{id}"),
            "policyVersion": "policy-v1",
            "modelVersion": "model-v1",
            "idempotencyKey": idem,
            "persistence": "PENDING",
            "occurredAtMs": 1_800_000_000_000_i64
        }),
    }
}

#[tokio::test]
async fn metadata_and_outbox_commit_atomically_and_survive_new_pool() {
    let Some(store) = store().await else { return };
    let item = input("restart");
    assert_eq!(store.commit_enrollment(&item).await.unwrap(), CommitOutcome::Created);
    let url = std::env::var("EYE_TEST_DATABASE_URL").unwrap();
    drop(store);
    let fresh_pool = connect(&url).await.unwrap();
    let fresh = EnrollmentOutboxStore::new(fresh_pool);
    let claimed = fresh.claim_due(item.created_at_ms + 1, 60_000, 4).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].event_id, item.event_id);
    assert_eq!(claimed[0].enrollment_id, item.enrollment_id);
    assert_eq!(claimed[0].attempts, 1);
}

#[tokio::test]
async fn same_idempotency_key_returns_existing_and_conflict_is_rejected() {
    let Some(store) = store().await else { return };
    let item = input("idem");
    assert_eq!(store.commit_enrollment(&item).await.unwrap(), CommitOutcome::Created);
    assert_eq!(store.commit_enrollment(&item).await.unwrap(), CommitOutcome::Existing);
    let mut changed = item.clone();
    changed.content_sha256 = "b".repeat(64);
    changed.payload["contentSha256"] = json!("b".repeat(64));
    assert_eq!(store.commit_enrollment(&changed).await, Err(OutboxStoreError::IdempotencyConflict));
}

#[tokio::test]
async fn broker_ack_is_required_before_delivered_state_and_replay_is_durable() {
    let Some(store) = store().await else { return };
    let item = input("ack");
    store.commit_enrollment(&item).await.unwrap();
    assert!(!store.mark_delivered(&item.event_id, item.created_at_ms + 2).await.unwrap());
    let claimed = store.claim_due(item.created_at_ms + 1, 60_000, 1).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert!(store.mark_delivered(&item.event_id, item.created_at_ms + 2).await.unwrap());
    assert!(!store.mark_delivered(&item.event_id, item.created_at_ms + 3).await.unwrap());
}

#[tokio::test]
async fn retry_exhaustion_dead_letters_and_operator_replay_are_atomic() {
    let Some(store) = store().await else { return };
    let item = input("replay");
    store.commit_enrollment(&item).await.unwrap();
    store.claim_due(item.created_at_ms + 1, 60_000, 1).await.unwrap();
    assert!(store.reschedule(&item.event_id, "KAFKA_UNAVAILABLE", item.created_at_ms + 2, 10, 1).await.unwrap());
    let replayed = store.replay_dead_letter(&item.event_id, "operator_123", item.created_at_ms + 3).await.unwrap();
    assert!(replayed);
    let claimed = store.claim_due(item.created_at_ms + 3, 60_000, 1).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].attempts, 1);
}

#[tokio::test]
async fn concurrent_claimers_do_not_receive_the_same_event() {
    let Some(store) = store().await else { return };
    let item = input("claim");
    store.commit_enrollment(&item).await.unwrap();
    let a = store.claim_due(item.created_at_ms + 1, 60_000, 1);
    let b = store.claim_due(item.created_at_ms + 1, 60_000, 1);
    let (a, b) = tokio::join!(a, b);
    assert_ne!(a.unwrap().len() + b.unwrap().len(), 2);
}

#[tokio::test]
async fn invalid_payload_is_rejected_before_database_write() {
    let Some(store) = store().await else { return };
    let mut item = input("invalid");
    item.payload["eventVersion"] = json!("9.9.9");
    assert_eq!(store.commit_enrollment(&item).await, Err(OutboxStoreError::Rejected));

    let mut sensitive = input("sensitive");
    sensitive.payload["embedding"] = json!([0.1, 0.2]);
    assert_eq!(store.commit_enrollment(&sensitive).await, Err(OutboxStoreError::Rejected));

    let mut mismatched = input("mismatch");
    mismatched.payload["dimensions"]["width"] = json!(1);
    assert_eq!(store.commit_enrollment(&mismatched).await, Err(OutboxStoreError::Rejected));
}

#[tokio::test]
async fn failed_outbox_insert_rolls_back_the_enrollment_row() {
    let Some(store) = store().await else { return };
    let first = input("first");
    store.commit_enrollment(&first).await.unwrap();
    let mut second = input("second");
    second.event_id = first.event_id.clone();
    second.payload["eventId"] = json!(first.event_id);
    assert!(store.commit_enrollment(&second).await.is_err());

    let url = std::env::var("EYE_TEST_DATABASE_URL").unwrap();
    let pool = connect(&url).await.unwrap();
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM eye_enrollment_records WHERE enrollment_id=$1",
    )
    .bind(&second.enrollment_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(count, 0, "metadata insert must roll back with outbox insert");
}
