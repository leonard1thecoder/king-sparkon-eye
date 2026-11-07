-- Canonical tenant registry and ownership constraints for Eye platform resources.
-- Legacy records are retained under a suspended, unowned tenant. They are never
-- silently assigned to a customer or made visible to newly provisioned clients.

CREATE TABLE IF NOT EXISTS eye_tenants (
    tenant_id TEXT PRIMARY KEY
        CHECK (tenant_id ~ '^ten_[0-9a-f]{32}$' OR tenant_id = 'ten_legacy_unassigned'),
    owner_user_id TEXT REFERENCES eye_auth_users(user_id) ON DELETE RESTRICT,
    display_name TEXT NOT NULL CHECK (length(display_name) BETWEEN 1 AND 120),
    status TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (status IN ('ACTIVE', 'SUSPENDED')),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    CONSTRAINT ck_eye_tenant_owner_state
        CHECK (owner_user_id IS NOT NULL OR status = 'SUSPENDED')
);

CREATE INDEX IF NOT EXISTS ix_eye_tenants_owner
    ON eye_tenants(owner_user_id, created_at_ms DESC)
    WHERE owner_user_id IS NOT NULL;

INSERT INTO eye_tenants (
    tenant_id, owner_user_id, display_name, status, created_at_ms, updated_at_ms
)
VALUES (
    'ten_legacy_unassigned', NULL, 'Legacy resources — unassigned', 'SUSPENDED', 1, 1
)
ON CONFLICT (tenant_id) DO NOTHING;

ALTER TABLE eye_enrollment_records
    ADD COLUMN IF NOT EXISTS tenant_id TEXT NOT NULL DEFAULT 'ten_legacy_unassigned';
ALTER TABLE eye_enrollment_records
    ALTER COLUMN tenant_id DROP DEFAULT;

ALTER TABLE eye_enrollment_records
    ADD CONSTRAINT uq_eye_enrollment_tenant_resource
    UNIQUE (tenant_id, enrollment_id);

ALTER TABLE eye_enrollment_records
    ADD CONSTRAINT fk_eye_enrollment_tenant
    FOREIGN KEY (tenant_id) REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT;

CREATE INDEX IF NOT EXISTS ix_eye_enrollment_tenant_created
    ON eye_enrollment_records(tenant_id, created_at_ms DESC);

ALTER TABLE eye_kafka_outbox
    ADD COLUMN IF NOT EXISTS tenant_id TEXT NOT NULL DEFAULT 'ten_legacy_unassigned';

UPDATE eye_kafka_outbox AS o
SET tenant_id = r.tenant_id
FROM eye_enrollment_records AS r
WHERE r.enrollment_id = o.enrollment_id
  AND o.tenant_id <> r.tenant_id;

ALTER TABLE eye_kafka_outbox
    ALTER COLUMN tenant_id DROP DEFAULT;

ALTER TABLE eye_kafka_outbox
    ADD CONSTRAINT uq_eye_outbox_tenant_event
    UNIQUE (tenant_id, event_id);

ALTER TABLE eye_kafka_outbox
    ADD CONSTRAINT fk_eye_outbox_tenant_enrollment
    FOREIGN KEY (tenant_id, enrollment_id)
    REFERENCES eye_enrollment_records(tenant_id, enrollment_id)
    ON DELETE RESTRICT;

CREATE INDEX IF NOT EXISTS ix_eye_outbox_tenant_due
    ON eye_kafka_outbox(tenant_id, status, next_attempt_at_ms, created_at_ms);

ALTER TABLE eye_kafka_dead_letters
    ADD COLUMN IF NOT EXISTS tenant_id TEXT NOT NULL DEFAULT 'ten_legacy_unassigned';

UPDATE eye_kafka_dead_letters AS d
SET tenant_id = o.tenant_id
FROM eye_kafka_outbox AS o
WHERE o.event_id = d.event_id
  AND d.tenant_id <> o.tenant_id;

ALTER TABLE eye_kafka_dead_letters
    ALTER COLUMN tenant_id DROP DEFAULT;

ALTER TABLE eye_kafka_dead_letters
    ADD CONSTRAINT fk_eye_dead_letter_tenant_event
    FOREIGN KEY (tenant_id, event_id)
    REFERENCES eye_kafka_outbox(tenant_id, event_id)
    ON DELETE RESTRICT;

CREATE INDEX IF NOT EXISTS ix_eye_dead_letters_tenant
    ON eye_kafka_dead_letters(tenant_id, dead_lettered_at_ms DESC);

COMMENT ON TABLE eye_tenants IS
    'Canonical Eye tenant registry. Active tenants must have an Eye account owner; legacy unassigned resources remain suspended.';
COMMENT ON COLUMN eye_enrollment_records.tenant_id IS
    'Trusted tenant ownership. Legacy rows are quarantined in ten_legacy_unassigned until an audited ownership migration.';
COMMENT ON COLUMN eye_kafka_outbox.tenant_id IS
    'Copied from the owning enrollment and constrained by a composite foreign key.';
COMMENT ON COLUMN eye_kafka_dead_letters.tenant_id IS
    'Copied from the owning outbox event and constrained by a composite foreign key.';
