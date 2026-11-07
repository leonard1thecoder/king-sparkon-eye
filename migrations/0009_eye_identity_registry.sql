-- Tenant-scoped generic identity registry (#185).
-- Identity rows contain only opaque references and bounded non-searchable metadata.
-- Biometric bytes/templates are deliberately stored outside this table.

CREATE TABLE IF NOT EXISTS eye_identities (
    identity_id TEXT PRIMARY KEY CHECK (identity_id ~ '^idy_[0-9a-f]{32}$'),
    tenant_id TEXT NOT NULL REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT,
    external_reference TEXT NOT NULL CHECK (
        length(external_reference) BETWEEN 1 AND 128
        AND external_reference = btrim(external_reference)
        AND external_reference !~ '[[:cntrl:]]'
    ),
    status TEXT NOT NULL DEFAULT 'ACTIVE'
        CHECK (status IN ('ACTIVE', 'REVOKED', 'DELETED')),
    enrollment_status TEXT NOT NULL DEFAULT 'NOT_ENROLLED'
        CHECK (enrollment_status IN ('NOT_ENROLLED', 'PENDING', 'PROCESSING', 'ACTIVE', 'FAILED', 'REVOKED')),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb
        CHECK (jsonb_typeof(metadata) = 'object' AND octet_length(metadata::text) <= 8192),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    revoked_at_ms BIGINT,
    deleted_at_ms BIGINT,
    CONSTRAINT uq_eye_identity_tenant_external UNIQUE (tenant_id, external_reference),
    CONSTRAINT uq_eye_identity_tenant_id UNIQUE (tenant_id, identity_id),
    CONSTRAINT ck_eye_identity_terminal_timestamps CHECK (
        (status <> 'REVOKED' OR revoked_at_ms IS NOT NULL)
        AND (status <> 'DELETED' OR deleted_at_ms IS NOT NULL)
    )
);

CREATE INDEX IF NOT EXISTS ix_eye_identities_tenant_status_created
    ON eye_identities(tenant_id, status, created_at_ms DESC);

CREATE TABLE IF NOT EXISTS eye_identity_audit (
    audit_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT,
    identity_id TEXT NOT NULL,
    client_id TEXT NOT NULL,
    actor_user_id TEXT NOT NULL REFERENCES eye_auth_users(user_id) ON DELETE RESTRICT,
    action TEXT NOT NULL CHECK (action IN ('IDENTITY_CREATED', 'IDENTITY_REVOKED', 'IDENTITY_DELETED')),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb CHECK (jsonb_typeof(metadata) = 'object'),
    CONSTRAINT fk_eye_identity_audit_identity
        FOREIGN KEY (tenant_id, identity_id)
        REFERENCES eye_identities(tenant_id, identity_id) ON DELETE RESTRICT,
    CONSTRAINT fk_eye_identity_audit_client
        FOREIGN KEY (tenant_id, client_id)
        REFERENCES eye_api_clients(tenant_id, client_id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS ix_eye_identity_audit_tenant_time
    ON eye_identity_audit(tenant_id, created_at_ms DESC);
