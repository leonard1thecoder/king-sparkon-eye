-- Tenant-owned machine clients and API credentials (#184).
-- Human sessions remain separate: API credentials authenticate machine clients only.

ALTER TABLE eye_tenants
    ADD CONSTRAINT uq_eye_tenants_tenant_owner UNIQUE (tenant_id, owner_user_id);

CREATE TABLE IF NOT EXISTS eye_api_clients (
    client_id TEXT PRIMARY KEY CHECK (client_id ~ '^cli_[0-9a-f]{32}$'),
    tenant_id TEXT NOT NULL,
    owner_user_id TEXT NOT NULL,
    display_name TEXT NOT NULL CHECK (length(display_name) BETWEEN 1 AND 120),
    scopes TEXT[] NOT NULL DEFAULT '{}'::text[] CHECK (cardinality(scopes) > 0 AND scopes <@ ARRAY['identities:read','identities:write','scopes:read','scopes:write','verification:create','verification:read','evidence:read','webhooks:manage']::text[]),
    status TEXT NOT NULL DEFAULT 'ACTIVE' CHECK (status IN ('ACTIVE', 'SUSPENDED')),
    rate_limit_per_minute INTEGER NOT NULL DEFAULT 600
        CHECK (rate_limit_per_minute BETWEEN 1 AND 100000),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    updated_at_ms BIGINT NOT NULL CHECK (updated_at_ms >= created_at_ms),
    CONSTRAINT fk_eye_api_client_tenant_owner
        FOREIGN KEY (tenant_id, owner_user_id)
        REFERENCES eye_tenants(tenant_id, owner_user_id) ON DELETE RESTRICT,
    CONSTRAINT fk_eye_api_client_owner
        FOREIGN KEY (owner_user_id) REFERENCES eye_auth_users(user_id) ON DELETE RESTRICT,
    CONSTRAINT uq_eye_api_client_tenant_id UNIQUE (tenant_id, client_id)
);

CREATE INDEX IF NOT EXISTS ix_eye_api_clients_tenant_status
    ON eye_api_clients(tenant_id, status, created_at_ms DESC);

CREATE TABLE IF NOT EXISTS eye_api_credentials (
    credential_id TEXT PRIMARY KEY CHECK (credential_id ~ '^crd_[0-9a-f]{32}$'),
    tenant_id TEXT NOT NULL,
    client_id TEXT NOT NULL,
    key_id TEXT NOT NULL UNIQUE CHECK (length(key_id) BETWEEN 16 AND 32),
    key_hash TEXT NOT NULL CHECK (key_hash ~ '^[0-9a-f]{64}$'),
    salt TEXT NOT NULL CHECK (salt ~ '^[0-9a-f]{32}$'),
    scopes TEXT[] NOT NULL DEFAULT '{}'::text[] CHECK (cardinality(scopes) > 0 AND scopes <@ ARRAY['identities:read','identities:write','scopes:read','scopes:write','verification:create','verification:read','evidence:read','webhooks:manage']::text[]),
    expires_at_ms BIGINT,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    last_used_at_ms BIGINT,
    revoked_at_ms BIGINT,
    rotated_from_credential_id TEXT,
    CONSTRAINT uq_eye_api_credential_tenant_id UNIQUE (tenant_id, credential_id),
    CONSTRAINT ck_eye_api_credential_expiry
        CHECK (expires_at_ms IS NULL OR expires_at_ms > created_at_ms),
    CONSTRAINT fk_eye_api_credential_client
        FOREIGN KEY (tenant_id, client_id)
        REFERENCES eye_api_clients(tenant_id, client_id) ON DELETE RESTRICT,
    CONSTRAINT fk_eye_api_credential_rotation
        FOREIGN KEY (tenant_id, rotated_from_credential_id)
        REFERENCES eye_api_credentials(tenant_id, credential_id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS ix_eye_api_credentials_tenant_client
    ON eye_api_credentials(tenant_id, client_id, created_at_ms DESC);
CREATE INDEX IF NOT EXISTS ix_eye_api_credentials_active
    ON eye_api_credentials(key_id, expires_at_ms)
    WHERE revoked_at_ms IS NULL;

CREATE TABLE IF NOT EXISTS eye_api_credential_audit (
    audit_id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id TEXT NOT NULL REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT,
    client_id TEXT NOT NULL,
    credential_id TEXT,
    actor_user_id TEXT NOT NULL REFERENCES eye_auth_users(user_id) ON DELETE RESTRICT,
    action TEXT NOT NULL CHECK (action IN ('CLIENT_CREATED', 'CREDENTIAL_CREATED', 'CREDENTIAL_REVOKED', 'CREDENTIAL_ROTATED')),
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0),
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT fk_eye_api_credential_audit_client
        FOREIGN KEY (tenant_id, client_id)
        REFERENCES eye_api_clients(tenant_id, client_id) ON DELETE RESTRICT,
    CONSTRAINT fk_eye_api_credential_audit_credential
        FOREIGN KEY (tenant_id, credential_id)
        REFERENCES eye_api_credentials(tenant_id, credential_id) ON DELETE RESTRICT
);
CREATE INDEX IF NOT EXISTS ix_eye_api_credential_audit_tenant_time
    ON eye_api_credential_audit(tenant_id, created_at_ms DESC);

CREATE TABLE IF NOT EXISTS eye_api_rate_limit_buckets (
    tenant_id TEXT NOT NULL REFERENCES eye_tenants(tenant_id) ON DELETE RESTRICT,
    dimension TEXT NOT NULL CHECK (dimension IN ('TENANT', 'CREDENTIAL')),
    subject_id TEXT NOT NULL,
    credential_id TEXT,
    window_start_ms BIGINT NOT NULL,
    request_count INTEGER NOT NULL CHECK (request_count > 0),
    PRIMARY KEY (tenant_id, dimension, subject_id, window_start_ms),
    CONSTRAINT fk_eye_api_rate_limit_credential
        FOREIGN KEY (tenant_id, credential_id)
        REFERENCES eye_api_credentials(tenant_id, credential_id) ON DELETE RESTRICT,
    CHECK (
        (dimension = 'TENANT' AND subject_id = tenant_id AND credential_id IS NULL)
        OR (dimension = 'CREDENTIAL' AND subject_id = credential_id AND credential_id IS NOT NULL)
    )
);
CREATE INDEX IF NOT EXISTS ix_eye_api_rate_limit_cleanup
    ON eye_api_rate_limit_buckets(window_start_ms);
