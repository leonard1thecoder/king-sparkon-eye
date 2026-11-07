-- Tenant-scoped evidence, audit, retention and retryable deletion controls (#189).
-- No default retention periods are invented: admission requires an explicit active policy.
CREATE TABLE IF NOT EXISTS eye_retention_policies (
    policy_id TEXT PRIMARY KEY CHECK (policy_id ~ '^ret_[0-9a-f]{32}$'),
    tenant_id TEXT NOT NULL REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT,
    data_class TEXT NOT NULL CHECK (data_class IN ('ORIGINAL_CAPTURE','FACE_TEMPLATE','EVIDENCE_METADATA','AUDIT_RECORD')),
    retention_ms BIGINT NOT NULL CHECK (retention_ms > 0),
    policy_version TEXT NOT NULL CHECK (length(policy_version) BETWEEN 1 AND 128 AND policy_version ~ '^[A-Za-z0-9_.-]+$'),
    effective_at_ms BIGINT NOT NULL CHECK (effective_at_ms > 0),
    active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    CONSTRAINT uq_eye_retention_policy_version UNIQUE (tenant_id,data_class,policy_version)
);
CREATE UNIQUE INDEX IF NOT EXISTS uq_eye_retention_policy_active
    ON eye_retention_policies(tenant_id,data_class) WHERE active=TRUE;
CREATE INDEX IF NOT EXISTS ix_eye_retention_policy_effective
    ON eye_retention_policies(tenant_id,data_class,effective_at_ms DESC) WHERE active=TRUE;

CREATE TABLE IF NOT EXISTS eye_audit_events (
    audit_event_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT,
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('USER','API_CLIENT','SYSTEM')),
    actor_ref TEXT NOT NULL CHECK (length(actor_ref) BETWEEN 1 AND 128 AND actor_ref !~ '[[:cntrl:]]'),
    action TEXT NOT NULL CHECK (action ~ '^[A-Z][A-Z0-9_]{1,79}$'),
    resource_kind TEXT NOT NULL CHECK (resource_kind ~ '^[A-Z][A-Z0-9_]{1,79}$'),
    resource_ref TEXT NOT NULL CHECK (length(resource_ref) BETWEEN 1 AND 128 AND resource_ref !~ '[[:cntrl:]]'),
    request_id TEXT CHECK (request_id IS NULL OR (length(request_id) BETWEEN 1 AND 128 AND request_id !~ '[[:cntrl:]]')),
    outcome TEXT NOT NULL CHECK (outcome IN ('SUCCESS','DENIED','FAILED')),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata)='object' AND octet_length(metadata::text)<=2048),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0)
);
CREATE INDEX IF NOT EXISTS ix_eye_audit_events_tenant_time
    ON eye_audit_events(tenant_id,created_at_ms DESC,audit_event_id DESC);
CREATE INDEX IF NOT EXISTS ix_eye_audit_events_tenant_resource
    ON eye_audit_events(tenant_id,resource_kind,resource_ref,created_at_ms DESC);

CREATE TABLE IF NOT EXISTS eye_evidence_references (
    evidence_id TEXT PRIMARY KEY CHECK (evidence_id ~ '^evd_[0-9a-f]{32}$'),
    tenant_id TEXT NOT NULL REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT,
    resource_kind TEXT NOT NULL CHECK (resource_kind IN ('IDENTITY','ENROLLMENT','VERIFICATION_SESSION')),
    resource_ref TEXT NOT NULL CHECK (length(resource_ref) BETWEEN 1 AND 128),
    data_class TEXT NOT NULL CHECK (data_class IN ('ORIGINAL_CAPTURE','FACE_TEMPLATE','SUPPORTING_EVIDENCE')),
    storage_key TEXT NOT NULL CHECK (length(storage_key) BETWEEN 1 AND 512 AND storage_key !~ '(^/|\\.\\.)'),
    status TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (status IN ('ACTIVE','DELETE_PENDING','DELETED')),
    retention_expires_at_ms BIGINT,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata)='object' AND octet_length(metadata::text)<=1024),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    deleted_at_ms BIGINT,
    CONSTRAINT uq_eye_evidence_tenant_resource UNIQUE (tenant_id,evidence_id),
    CONSTRAINT ck_eye_evidence_deleted CHECK ((status='DELETED' AND deleted_at_ms IS NOT NULL) OR (status<>'DELETED' AND deleted_at_ms IS NULL))
);
CREATE INDEX IF NOT EXISTS ix_eye_evidence_retention_due
    ON eye_evidence_references(tenant_id,retention_expires_at_ms)
    WHERE status='ACTIVE' AND retention_expires_at_ms IS NOT NULL;
CREATE INDEX IF NOT EXISTS ix_eye_evidence_resource
    ON eye_evidence_references(tenant_id,resource_kind,resource_ref);

CREATE TABLE IF NOT EXISTS eye_deletion_requests (
    deletion_id TEXT PRIMARY KEY CHECK (deletion_id ~ '^del_[0-9a-f]{32}$'),
    tenant_id TEXT NOT NULL REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT,
    target_kind TEXT NOT NULL CHECK (target_kind IN ('IDENTITY','ENROLLMENT','VERIFICATION_SESSION','EVIDENCE','FACE_TEMPLATE')),
    target_ref TEXT NOT NULL CHECK (length(target_ref) BETWEEN 1 AND 128),
    reason_code TEXT NOT NULL CHECK (reason_code IN ('SUBJECT_REQUEST','CONSENT_WITHDRAWN','RETENTION_EXPIRED','TENANT_REQUEST','SECURITY_REMEDIATION')),
    actor_kind TEXT NOT NULL CHECK (actor_kind IN ('USER','API_CLIENT','SYSTEM')),
    actor_ref TEXT NOT NULL CHECK (length(actor_ref) BETWEEN 1 AND 128),
    idempotency_key TEXT NOT NULL CHECK (idempotency_key ~ '^[A-Za-z0-9_-]{1,128}$'),
    status TEXT NOT NULL DEFAULT 'QUEUED' CHECK (status IN ('QUEUED','PROCESSING','COMPLETED','FAILED')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts BETWEEN 0 AND 20),
    max_attempts INTEGER NOT NULL DEFAULT 8 CHECK (max_attempts BETWEEN 1 AND 20),
    next_attempt_at_ms BIGINT NOT NULL CHECK (next_attempt_at_ms > 0),
    lease_token UUID,
    worker_id TEXT CHECK (worker_id IS NULL OR worker_id ~ '^[A-Za-z0-9_.:-]{1,128}$'),
    lease_until_ms BIGINT,
    last_error_category TEXT CHECK (last_error_category IS NULL OR last_error_category ~ '^[A-Z0-9_]{1,80}$'),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    completed_at_ms BIGINT,
    CONSTRAINT uq_eye_deletion_idempotency UNIQUE (tenant_id,target_kind,target_ref,idempotency_key),
    CONSTRAINT ck_eye_deletion_lease CHECK (
        (status='PROCESSING' AND lease_token IS NOT NULL AND worker_id IS NOT NULL AND lease_until_ms IS NOT NULL)
        OR (status<>'PROCESSING' AND lease_token IS NULL AND worker_id IS NULL AND lease_until_ms IS NULL)
    ),
    CONSTRAINT ck_eye_deletion_terminal CHECK (
        (status='COMPLETED' AND completed_at_ms IS NOT NULL)
        OR (status<>'COMPLETED' AND completed_at_ms IS NULL)
    )
);
CREATE INDEX IF NOT EXISTS ix_eye_deletion_claim
    ON eye_deletion_requests(tenant_id,status,next_attempt_at_ms,created_at_ms)
    WHERE status IN ('QUEUED','PROCESSING');

-- A revoked/deleted identity immediately fences evidence references for deletion.
-- Object removal is asynchronous; the durable deletion request performs retryable cleanup.
CREATE OR REPLACE FUNCTION eye_queue_identity_evidence_deletion()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE evidence_row RECORD;
BEGIN
    IF NEW.status NOT IN ('REVOKED','DELETED') OR OLD.status IN ('REVOKED','DELETED') THEN
        RETURN NEW;
    END IF;
    FOR evidence_row IN
        UPDATE eye_evidence_references
           SET status='DELETE_PENDING'
         WHERE tenant_id=NEW.tenant_id
           AND resource_kind='IDENTITY'
           AND resource_ref=NEW.identity_id
           AND status='ACTIVE'
         RETURNING evidence_id
    LOOP
        INSERT INTO eye_deletion_requests(
            deletion_id,tenant_id,target_kind,target_ref,reason_code,actor_kind,actor_ref,
            idempotency_key,status,next_attempt_at_ms,created_at_ms,updated_at_ms
        ) VALUES (
            'del_' || md5(NEW.tenant_id || ':' || evidence_row.evidence_id),
            NEW.tenant_id,'EVIDENCE',evidence_row.evidence_id,'SECURITY_REMEDIATION',
            'SYSTEM','identity-lifecycle','identity-' || evidence_row.evidence_id,
            'QUEUED',NEW.updated_at_ms,NEW.updated_at_ms,NEW.updated_at_ms
        ) ON CONFLICT (tenant_id,target_kind,target_ref,idempotency_key) DO NOTHING;
    END LOOP;
    RETURN NEW;
END;
$$;
DROP TRIGGER IF EXISTS trg_eye_queue_identity_evidence_deletion ON eye_identities;
CREATE TRIGGER trg_eye_queue_identity_evidence_deletion
AFTER UPDATE OF status ON eye_identities
FOR EACH ROW EXECUTE FUNCTION eye_queue_identity_evidence_deletion();
