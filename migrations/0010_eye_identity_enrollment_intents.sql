-- Durable face-enrollment intent state for #185, with job execution owned by #187.
-- No image, embedding or template bytes are accepted or persisted while #95 is blocked.

ALTER TABLE eye_identities
    DROP CONSTRAINT IF EXISTS eye_identities_enrollment_status_check;

ALTER TABLE eye_identities
    ADD CONSTRAINT eye_identities_enrollment_status_check
    CHECK (enrollment_status IN (
        'NOT_ENROLLED', 'PENDING', 'PROCESSING', 'ACTIVE',
        'FAILED', 'REVOKED', 'BLOCKED_EVALUATION'
    ));

ALTER TABLE eye_identity_audit
    DROP CONSTRAINT IF EXISTS eye_identity_audit_action_check;

ALTER TABLE eye_identity_audit
    ADD CONSTRAINT eye_identity_audit_action_check
    CHECK (action IN (
        'IDENTITY_CREATED', 'IDENTITY_REVOKED', 'IDENTITY_DELETED',
        'ENROLLMENT_BLOCKED', 'ENROLLMENT_REVOKED', 'ENROLLMENT_DELETED'
    ));

CREATE TABLE IF NOT EXISTS eye_identity_enrollments (
    enrollment_id TEXT PRIMARY KEY CHECK (enrollment_id ~ '^enr_[0-9a-f]{32}$'),
    tenant_id TEXT NOT NULL,
    identity_id TEXT NOT NULL,
    client_id TEXT NOT NULL,
    actor_user_id TEXT NOT NULL REFERENCES eye_auth_users(user_id) ON DELETE RESTRICT,
    consent_reference TEXT NOT NULL CHECK (
        length(consent_reference) BETWEEN 8 AND 128
        AND consent_reference ~ '^[A-Za-z0-9_-]+$'
    ),
    consent_policy_version TEXT NOT NULL CHECK (
        length(consent_policy_version) BETWEEN 1 AND 128
        AND consent_policy_version ~ '^[A-Za-z0-9_.-]+$'
    ),
    idempotency_key TEXT NOT NULL CHECK (
        length(idempotency_key) BETWEEN 1 AND 128
        AND idempotency_key ~ '^[A-Za-z0-9_-]+$'
    ),
    status TEXT NOT NULL CHECK (status IN (
        'BLOCKED_EVALUATION', 'QUEUED', 'PROCESSING', 'FAILED',
        'ACTIVE', 'REVOKED', 'DELETED'
    )),
    blocked_reason TEXT CHECK (
        blocked_reason IS NULL OR blocked_reason IN ('EVALUATION_DATA_REQUIRED', 'RETENTION_POLICY_REQUIRED', 'CONSENT_AUTHORITY_UNAVAILABLE')
    ),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    CONSTRAINT fk_eye_identity_enrollment_identity
        FOREIGN KEY (tenant_id, identity_id)
        REFERENCES eye_identities(tenant_id, identity_id) ON DELETE RESTRICT,
    CONSTRAINT fk_eye_identity_enrollment_client
        FOREIGN KEY (tenant_id, client_id)
        REFERENCES eye_api_clients(tenant_id, client_id) ON DELETE RESTRICT,
    CONSTRAINT uq_eye_identity_enrollment_idempotency
        UNIQUE (tenant_id, identity_id, idempotency_key),
    CONSTRAINT uq_eye_identity_enrollment_tenant_id
        UNIQUE (tenant_id, enrollment_id),
    CONSTRAINT ck_eye_identity_enrollment_block_reason
        CHECK (
            (status = 'BLOCKED_EVALUATION' AND blocked_reason IS NOT NULL)
            OR (status <> 'BLOCKED_EVALUATION' AND blocked_reason IS NULL)
        )
);

CREATE INDEX IF NOT EXISTS ix_eye_identity_enrollments_tenant_identity_created
    ON eye_identity_enrollments(tenant_id, identity_id, created_at_ms DESC);

CREATE INDEX IF NOT EXISTS ix_eye_identity_enrollments_blocked
    ON eye_identity_enrollments(status, created_at_ms)
    WHERE status = 'BLOCKED_EVALUATION';

-- Revoking or deleting an identity also revokes every associated enrollment intent.
CREATE OR REPLACE FUNCTION eye_revoke_identity_enrollments()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    enrollment_row RECORD;
    audit_action TEXT;
BEGIN
    IF NEW.status = 'DELETED' AND OLD.status <> 'DELETED' THEN
        audit_action := 'ENROLLMENT_DELETED';
    ELSIF NEW.status = 'REVOKED' AND OLD.status = 'ACTIVE' THEN
        audit_action := 'ENROLLMENT_REVOKED';
    ELSE
        RETURN NEW;
    END IF;

    FOR enrollment_row IN
        UPDATE eye_identity_enrollments
           SET status = CASE WHEN NEW.status = 'DELETED' THEN 'DELETED' ELSE 'REVOKED' END,
               blocked_reason = NULL,
               updated_at_ms = NEW.updated_at_ms
         WHERE tenant_id = NEW.tenant_id
           AND identity_id = NEW.identity_id
           AND status NOT IN ('REVOKED', 'DELETED')
         RETURNING enrollment_id, client_id, actor_user_id
    LOOP
        INSERT INTO eye_identity_audit (
            tenant_id, identity_id, client_id, actor_user_id, action, created_at_ms, metadata
        ) VALUES (
            NEW.tenant_id, NEW.identity_id, enrollment_row.client_id,
            enrollment_row.actor_user_id, audit_action, NEW.updated_at_ms,
            jsonb_build_object('enrollmentId', enrollment_row.enrollment_id)
        );
    END LOOP;
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS trg_eye_revoke_identity_enrollments ON eye_identities;
CREATE TRIGGER trg_eye_revoke_identity_enrollments
AFTER UPDATE OF status ON eye_identities
FOR EACH ROW EXECUTE FUNCTION eye_revoke_identity_enrollments();
