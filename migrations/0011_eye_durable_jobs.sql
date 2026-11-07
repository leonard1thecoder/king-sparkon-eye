-- Generic tenant-scoped durable processing jobs and result-event outbox (#187).
-- This is separate from the legacy photo.enrolled.v1 Kafka publisher in #113.
CREATE TABLE IF NOT EXISTS eye_processing_jobs (
    job_id TEXT PRIMARY KEY CHECK (job_id ~ '^job_[0-9a-f]{32}$'),
    tenant_id TEXT NOT NULL REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT,
    resource_type TEXT NOT NULL CHECK (resource_type IN ('IDENTITY_ENROLLMENT', 'VERIFICATION_SESSION')),
    resource_id TEXT NOT NULL CHECK (length(resource_id) BETWEEN 1 AND 128),
    idempotency_key TEXT NOT NULL CHECK (idempotency_key ~ '^[A-Za-z0-9_-]{1,128}$'),
    status TEXT NOT NULL DEFAULT 'QUEUED'
        CHECK (status IN ('QUEUED','PROCESSING','COMPLETED','FAILED','EXPIRED','CANCELLED')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    max_attempts INTEGER NOT NULL DEFAULT 5 CHECK (max_attempts BETWEEN 1 AND 20),
    available_at_ms BIGINT NOT NULL CHECK (available_at_ms > 0),
    lease_token UUID,
    worker_id TEXT CHECK (worker_id IS NULL OR worker_id ~ '^[A-Za-z0-9_.:-]{1,128}$'),
    lease_until_ms BIGINT,
    heartbeat_at_ms BIGINT,
    input_metadata JSONB NOT NULL DEFAULT '{}'::jsonb
        CHECK (jsonb_typeof(input_metadata) = 'object' AND octet_length(input_metadata::text) <= 4096),
    result JSONB CHECK (result IS NULL OR (jsonb_typeof(result) = 'object' AND octet_length(result::text) <= 8192)),
    error_category TEXT CHECK (error_category IS NULL OR error_category ~ '^[A-Z0-9_]{1,80}$'),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    started_at_ms BIGINT,
    completed_at_ms BIGINT,
    CONSTRAINT uq_eye_processing_job_idempotency UNIQUE (tenant_id, resource_type, resource_id, idempotency_key),
    CONSTRAINT uq_eye_processing_job_tenant_id UNIQUE (tenant_id, job_id),
    CONSTRAINT ck_eye_processing_job_lease CHECK (
        (status = 'PROCESSING' AND lease_token IS NOT NULL AND worker_id IS NOT NULL AND lease_until_ms IS NOT NULL)
        OR (status <> 'PROCESSING' AND lease_token IS NULL AND worker_id IS NULL AND lease_until_ms IS NULL)
    ),
    CONSTRAINT ck_eye_processing_job_terminal CHECK (
        (status IN ('COMPLETED','FAILED','EXPIRED','CANCELLED') AND completed_at_ms IS NOT NULL)
        OR (status IN ('QUEUED','PROCESSING') AND completed_at_ms IS NULL)
    )
);
CREATE INDEX IF NOT EXISTS ix_eye_processing_jobs_claim
    ON eye_processing_jobs(status, available_at_ms, created_at_ms)
    WHERE status IN ('QUEUED','PROCESSING');
CREATE INDEX IF NOT EXISTS ix_eye_processing_jobs_tenant_resource
    ON eye_processing_jobs(tenant_id, resource_type, resource_id, created_at_ms DESC);

CREATE TABLE IF NOT EXISTS eye_platform_outbox (
    event_id TEXT PRIMARY KEY CHECK (event_id ~ '^evt_[0-9a-f]{32}$'),
    tenant_id TEXT NOT NULL REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT,
    job_id TEXT NOT NULL REFERENCES eye_processing_jobs(job_id) ON DELETE RESTRICT,
    event_type TEXT NOT NULL CHECK (event_type IN ('job.completed.v1','job.failed.v1','job.expired.v1','job.cancelled.v1')),
    payload JSONB NOT NULL CHECK (jsonb_typeof(payload) = 'object' AND octet_length(payload::text) <= 8192),
    status TEXT NOT NULL DEFAULT 'PENDING' CHECK (status IN ('PENDING','IN_FLIGHT','DELIVERED','DEAD_LETTER')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at_ms BIGINT NOT NULL CHECK (next_attempt_at_ms > 0),
    lease_until_ms BIGINT,
    lease_token UUID,
    last_error_category TEXT CHECK (last_error_category IS NULL OR last_error_category ~ '^[A-Z0-9_]{1,80}$'),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    delivered_at_ms BIGINT,
    CONSTRAINT uq_eye_platform_outbox_job_event UNIQUE (job_id, event_type),
    CONSTRAINT fk_eye_platform_outbox_job_tenant FOREIGN KEY (tenant_id, job_id) REFERENCES eye_processing_jobs(tenant_id, job_id) ON DELETE RESTRICT,
    CONSTRAINT ck_eye_platform_outbox_lease CHECK (
        (status = 'IN_FLIGHT' AND lease_until_ms IS NOT NULL AND lease_token IS NOT NULL)
        OR (status <> 'IN_FLIGHT' AND lease_until_ms IS NULL AND lease_token IS NULL)
    )
);
CREATE INDEX IF NOT EXISTS ix_eye_platform_outbox_due
    ON eye_platform_outbox(status, next_attempt_at_ms, created_at_ms)
    WHERE status IN ('PENDING','IN_FLIGHT');
