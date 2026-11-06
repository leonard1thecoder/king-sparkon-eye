-- Durable, tenant-scoped billing usage ledger and transactional reporting intent.
-- This stores opaque tenant/event references and meter quantities only. No biometric or business payloads.

CREATE TABLE IF NOT EXISTS eye_billing_usage_ledger (
    id BIGSERIAL PRIMARY KEY,
    tenant_id TEXT NOT NULL CHECK (tenant_id ~ '^[A-Za-z0-9_-]{1,64}$'),
    event_key TEXT NOT NULL CHECK (event_key ~ '^[A-Za-z0-9_.:-]{1,128}$'),
    source TEXT NOT NULL CHECK (source IN (
        'confirmed_enrollment',
        'eligible_identification_completed',
        'template_sample',
        'photo_storage_sample'
    )),
    meter TEXT NOT NULL CHECK (meter IN (
        'enrollments',
        'identification_requests',
        'active_templates',
        'retained_photo_storage_gb'
    )),
    occurred_at TIMESTAMPTZ NOT NULL,
    period_start DATE NOT NULL,
    body_kind TEXT NOT NULL CHECK (body_kind IN ('flow', 'level', 'reversal')),
    quantity BIGINT,
    reversal_of_event_key TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_eye_billing_usage_tenant_event UNIQUE (tenant_id, event_key),
    CONSTRAINT fk_eye_billing_usage_reversal
        FOREIGN KEY (tenant_id, reversal_of_event_key)
        REFERENCES eye_billing_usage_ledger (tenant_id, event_key),
    CONSTRAINT ck_eye_billing_usage_body CHECK (
        (body_kind = 'flow' AND quantity > 0 AND reversal_of_event_key IS NULL
            AND source IN ('confirmed_enrollment', 'eligible_identification_completed'))
        OR
        (body_kind = 'level' AND quantity >= 0 AND reversal_of_event_key IS NULL
            AND source IN ('template_sample', 'photo_storage_sample'))
        OR
        (body_kind = 'reversal' AND quantity IS NULL AND reversal_of_event_key IS NOT NULL
            AND source IN ('confirmed_enrollment', 'eligible_identification_completed'))
    ),
    CONSTRAINT ck_eye_billing_usage_source_meter CHECK (
        (source = 'confirmed_enrollment' AND meter = 'enrollments')
        OR (source = 'eligible_identification_completed' AND meter = 'identification_requests')
        OR (source = 'template_sample' AND meter = 'active_templates')
        OR (source = 'photo_storage_sample' AND meter = 'retained_photo_storage_gb')
    )
);

CREATE UNIQUE INDEX IF NOT EXISTS uq_eye_billing_usage_reversal_once
    ON eye_billing_usage_ledger (tenant_id, reversal_of_event_key)
    WHERE body_kind = 'reversal';

CREATE INDEX IF NOT EXISTS ix_eye_billing_usage_period
    ON eye_billing_usage_ledger (tenant_id, period_start, meter, occurred_at);

CREATE TABLE IF NOT EXISTS eye_billing_usage_outbox (
    id BIGSERIAL PRIMARY KEY,
    ledger_entry_id BIGINT NOT NULL UNIQUE
        REFERENCES eye_billing_usage_ledger(id) ON DELETE RESTRICT,
    tenant_id TEXT NOT NULL CHECK (tenant_id ~ '^[A-Za-z0-9_-]{1,64}$'),
    meter TEXT NOT NULL CHECK (meter IN (
        'enrollments',
        'identification_requests',
        'active_templates',
        'retained_photo_storage_gb'
    )),
    period_start DATE NOT NULL,
    status TEXT NOT NULL DEFAULT 'PENDING'
        CHECK (status IN ('PENDING', 'IN_FLIGHT', 'REPORTED', 'DEAD_LETTER')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    lease_until TIMESTAMPTZ,
    last_error_category TEXT CHECK (
        last_error_category IS NULL OR length(last_error_category) BETWEEN 1 AND 80
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ix_eye_billing_usage_outbox_due
    ON eye_billing_usage_outbox (status, next_attempt_at, id);
