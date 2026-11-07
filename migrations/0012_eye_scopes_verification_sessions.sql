-- Generic tenant-isolated identity scopes and asynchronous verification session resources (#186).
-- Session metadata only: capture bytes/templates/similarity scores are not accepted or stored here.
CREATE TABLE IF NOT EXISTS eye_identity_scopes (
    scope_id TEXT PRIMARY KEY CHECK (scope_id ~ '^scp_[0-9a-f]{32}$'),
    tenant_id TEXT NOT NULL REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT,
    external_reference TEXT NOT NULL CHECK (length(external_reference) BETWEEN 1 AND 128 AND external_reference = btrim(external_reference) AND external_reference !~ '[[:cntrl:]]'),
    display_name TEXT NOT NULL CHECK (length(display_name) BETWEEN 1 AND 120 AND display_name = btrim(display_name)),
    description TEXT CHECK (description IS NULL OR length(description) <= 500),
    status TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (status IN ('ACTIVE','DELETED')),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    deleted_at_ms BIGINT,
    CONSTRAINT uq_eye_scope_tenant_external UNIQUE (tenant_id, external_reference),
    CONSTRAINT uq_eye_scope_tenant_id UNIQUE (tenant_id, scope_id),
    CONSTRAINT ck_eye_scope_deleted CHECK ((status='DELETED' AND deleted_at_ms IS NOT NULL) OR (status='ACTIVE' AND deleted_at_ms IS NULL))
);
CREATE INDEX IF NOT EXISTS ix_eye_identity_scopes_tenant_status ON eye_identity_scopes(tenant_id,status,created_at_ms DESC);

CREATE TABLE IF NOT EXISTS eye_identity_scope_members (
    tenant_id TEXT NOT NULL,
    scope_id TEXT NOT NULL,
    identity_id TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    removed_at_ms BIGINT,
    CONSTRAINT pk_eye_identity_scope_members PRIMARY KEY (tenant_id,scope_id,identity_id),
    CONSTRAINT fk_eye_scope_member_scope FOREIGN KEY (tenant_id,scope_id) REFERENCES eye_identity_scopes(tenant_id,scope_id) ON DELETE RESTRICT,
    CONSTRAINT fk_eye_scope_member_identity FOREIGN KEY (tenant_id,identity_id) REFERENCES eye_identities(tenant_id,identity_id) ON DELETE RESTRICT,
    CONSTRAINT ck_eye_scope_member_removal CHECK (removed_at_ms IS NULL OR removed_at_ms >= created_at_ms)
);
CREATE INDEX IF NOT EXISTS ix_eye_scope_members_identity ON eye_identity_scope_members(tenant_id,identity_id) WHERE removed_at_ms IS NULL;

CREATE TABLE IF NOT EXISTS eye_verification_sessions (
    session_id TEXT PRIMARY KEY CHECK (session_id ~ '^vfy_[0-9a-f]{32}$'),
    tenant_id TEXT NOT NULL REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT,
    client_id TEXT NOT NULL,
    mode TEXT NOT NULL CHECK (mode IN ('MATCH','IDENTIFY')),
    identity_id TEXT,
    scope_id TEXT,
    status TEXT NOT NULL DEFAULT 'AWAITING_CAPTURE' CHECK (status IN ('AWAITING_CAPTURE','BLOCKED_EVALUATION','QUEUED','PROCESSING','COMPLETED','FAILED','EXPIRED','CANCELLED')),
    outcome TEXT CHECK (outcome IS NULL OR outcome IN ('VERIFIED','NOT_VERIFIED','IDENTIFIED','NO_CANDIDATE','AMBIGUOUS','INCONCLUSIVE')),
    reason_code TEXT CHECK (reason_code IS NULL OR reason_code IN ('EVALUATION_DATA_REQUIRED','RETENTION_POLICY_REQUIRED','CONSENT_AUTHORITY_UNAVAILABLE','CAPTURE_INVALID','PROCESSING_UNAVAILABLE','SESSION_EXPIRED','SESSION_CANCELLED')),
    purpose TEXT CHECK (purpose IS NULL OR length(purpose) <= 128),
    context JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(context)='object' AND octet_length(context::text) <= 2048),
    idempotency_key TEXT NOT NULL CHECK (idempotency_key ~ '^[A-Za-z0-9_-]{1,128}$'),
    expires_at_ms BIGINT NOT NULL CHECK (expires_at_ms > 0),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    completed_at_ms BIGINT,
    CONSTRAINT fk_eye_verification_session_client FOREIGN KEY (tenant_id,client_id) REFERENCES eye_api_clients(tenant_id,client_id) ON DELETE RESTRICT,
    CONSTRAINT fk_eye_verification_session_identity FOREIGN KEY (tenant_id,identity_id) REFERENCES eye_identities(tenant_id,identity_id) ON DELETE RESTRICT,
    CONSTRAINT fk_eye_verification_session_scope FOREIGN KEY (tenant_id,scope_id) REFERENCES eye_identity_scopes(tenant_id,scope_id) ON DELETE RESTRICT,
    CONSTRAINT uq_eye_verification_session_idempotency UNIQUE (tenant_id,idempotency_key),
    CONSTRAINT uq_eye_verification_session_tenant_id UNIQUE (tenant_id,session_id),
    CONSTRAINT ck_eye_verification_session_target CHECK (
        (mode='MATCH' AND identity_id IS NOT NULL AND scope_id IS NULL)
        OR (mode='IDENTIFY' AND scope_id IS NOT NULL AND identity_id IS NULL)
    ),
    CONSTRAINT ck_eye_verification_session_outcome CHECK (
        (status='COMPLETED' AND outcome IS NOT NULL AND completed_at_ms IS NOT NULL)
        OR (status<>'COMPLETED' AND outcome IS NULL)
    )
);
CREATE INDEX IF NOT EXISTS ix_eye_verification_sessions_expiry ON eye_verification_sessions(status,expires_at_ms) WHERE status IN ('AWAITING_CAPTURE','BLOCKED_EVALUATION','QUEUED','PROCESSING');
CREATE INDEX IF NOT EXISTS ix_eye_verification_sessions_tenant_created ON eye_verification_sessions(tenant_id,created_at_ms DESC);

CREATE TABLE IF NOT EXISTS eye_verification_session_audit (
    audit_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    client_id TEXT NOT NULL,
    action TEXT NOT NULL CHECK (action IN ('SESSION_CREATED','SESSION_EXPIRED','SESSION_CANCELLED','CAPTURE_BLOCKED_EVALUATION')),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata)='object' AND octet_length(metadata::text) <= 1024),
    FOREIGN KEY (tenant_id,session_id) REFERENCES eye_verification_sessions(tenant_id,session_id) ON DELETE RESTRICT,
    FOREIGN KEY (tenant_id,client_id) REFERENCES eye_api_clients(tenant_id,client_id) ON DELETE RESTRICT
);
