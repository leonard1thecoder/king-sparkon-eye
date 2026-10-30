-- Eye-owned durable enrollment metadata and transactional event outbox.
-- This migration is applied by SQLx to the Eye PostgreSQL database, not Tracker's database.

CREATE TABLE IF NOT EXISTS eye_enrollment_records (
    enrollment_id TEXT PRIMARY KEY CHECK (enrollment_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    consent_reference TEXT NOT NULL CHECK (consent_reference ~ '^[A-Za-z0-9_-]{8,128}$'),
    object_key TEXT NOT NULL CHECK (
        object_key LIKE 'enrollments/%' AND length(object_key) <= 512
        AND position('..' IN object_key) = 0 AND position('://' IN object_key) = 0
    ),
    content_sha256 CHAR(64) NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'),
    byte_len BIGINT NOT NULL CHECK (byte_len > 0),
    image_width INTEGER NOT NULL CHECK (image_width > 0),
    image_height INTEGER NOT NULL CHECK (image_height > 0),
    policy_version TEXT NOT NULL CHECK (policy_version ~ '^[A-Za-z0-9_.-]{1,128}$'),
    model_version TEXT CHECK (model_version IS NULL OR model_version ~ '^[A-Za-z0-9_.-]{1,128}$'),
    idempotency_key TEXT UNIQUE CHECK (idempotency_key IS NULL OR idempotency_key ~ '^[A-Za-z0-9_-]{1,128}$'),
    status TEXT NOT NULL CHECK (status = 'STORED'),
    persistence_status TEXT NOT NULL CHECK (persistence_status = 'PENDING'),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0)
);

CREATE TABLE IF NOT EXISTS eye_kafka_outbox (
    event_id TEXT PRIMARY KEY CHECK (event_id ~ '^[A-Za-z0-9_-]{1,128}$'),
    enrollment_id TEXT NOT NULL UNIQUE REFERENCES eye_enrollment_records(enrollment_id),
    topic TEXT NOT NULL CHECK (topic = 'eye.enrollments.v1'),
    partition_key TEXT NOT NULL,
    payload JSONB NOT NULL CHECK (jsonb_typeof(payload) = 'object'),
    status TEXT NOT NULL DEFAULT 'PENDING'
        CHECK (status IN ('PENDING', 'IN_FLIGHT', 'DELIVERED', 'DEAD_LETTER')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at_ms BIGINT NOT NULL,
    lease_until_ms BIGINT,
    last_error_category TEXT CHECK (last_error_category IS NULL OR length(last_error_category) <= 80),
    created_at_ms BIGINT NOT NULL,
    delivered_at_ms BIGINT,
    updated_at_ms BIGINT NOT NULL,
    CHECK (partition_key = enrollment_id),
    CHECK (payload->>'eventId' = event_id),
    CHECK (payload->>'enrollmentId' = enrollment_id),
    CHECK (payload->>'eventType' = 'photo.enrolled.v1'),
    CHECK (payload->>'eventVersion' = '1.0.0')
);

CREATE INDEX IF NOT EXISTS ix_eye_outbox_due
    ON eye_kafka_outbox(status, next_attempt_at_ms, lease_until_ms, created_at_ms);

CREATE TABLE IF NOT EXISTS eye_kafka_dead_letters (
    event_id TEXT PRIMARY KEY REFERENCES eye_kafka_outbox(event_id),
    error_category TEXT NOT NULL CHECK (length(error_category) BETWEEN 1 AND 80),
    attempts INTEGER NOT NULL CHECK (attempts >= 0),
    dead_lettered_at_ms BIGINT NOT NULL,
    replayed_at_ms BIGINT,
    replayed_by TEXT CHECK (replayed_by IS NULL OR replayed_by ~ '^[A-Za-z0-9_-]{1,128}$')
);
