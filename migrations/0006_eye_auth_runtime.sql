-- Complete Eye-owned SQLx auth runtime state.
-- These tables are independent from Tracker/Spring Boot authentication storage.
CREATE TABLE IF NOT EXISTS eye_auth_users (
    user_id TEXT PRIMARY KEY CHECK (user_id ~ '^usr_[A-Za-z0-9_-]{8,80}$'),
    email TEXT NOT NULL,
    password_hash TEXT NOT NULL,
    salt TEXT NOT NULL DEFAULT '',
    role TEXT NOT NULL CHECK (role IN ('GateOperator', 'Admin', 'Auditor')),
    scopes TEXT[] NOT NULL DEFAULT '{}'::text[],
    is_active BOOLEAN NOT NULL DEFAULT TRUE,
    created_at_ms BIGINT NOT NULL CHECK (created_at_ms > 0)
);
CREATE UNIQUE INDEX IF NOT EXISTS eye_auth_users_email_lower_uidx
    ON eye_auth_users (lower(email));

ALTER TABLE identity_links
    ADD CONSTRAINT identity_links_user_id_fk
    FOREIGN KEY (user_id) REFERENCES eye_auth_users(user_id) ON DELETE CASCADE;

ALTER TABLE oauth_transactions
    ADD COLUMN nonce_enc BYTEA NOT NULL DEFAULT ''::bytea;
ALTER TABLE oauth_transactions
    ALTER COLUMN nonce_enc DROP DEFAULT;

CREATE TABLE IF NOT EXISTS eye_refresh_tokens (
    token_hash BYTEA PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    family_id TEXT NOT NULL CHECK (char_length(family_id) BETWEEN 8 AND 128),
    user_id TEXT NOT NULL REFERENCES eye_auth_users(user_id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('GateOperator', 'Admin', 'Auditor')),
    scopes TEXT[] NOT NULL DEFAULT '{}'::text[],
    event_id TEXT,
    issued_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    used_at_ms BIGINT,
    revoked_at_ms BIGINT,
    CHECK (expires_at_ms > issued_at_ms)
);
CREATE INDEX IF NOT EXISTS eye_refresh_tokens_family_idx ON eye_refresh_tokens(family_id);
CREATE INDEX IF NOT EXISTS eye_refresh_tokens_expiry_idx ON eye_refresh_tokens(expires_at_ms);

CREATE TABLE IF NOT EXISTS eye_password_reset_tokens (
    token_hash BYTEA PRIMARY KEY CHECK (octet_length(token_hash) = 32),
    user_id TEXT NOT NULL REFERENCES eye_auth_users(user_id) ON DELETE CASCADE,
    created_at_ms BIGINT NOT NULL,
    expires_at_ms BIGINT NOT NULL,
    consumed_at_ms BIGINT,
    CHECK (expires_at_ms > created_at_ms)
);
CREATE INDEX IF NOT EXISTS eye_password_reset_tokens_user_idx
    ON eye_password_reset_tokens(user_id);
CREATE INDEX IF NOT EXISTS eye_password_reset_tokens_expiry_idx
    ON eye_password_reset_tokens(expires_at_ms);

CREATE TABLE IF NOT EXISTS eye_oauth_rate_limits (
    bucket_hash BYTEA NOT NULL CHECK (octet_length(bucket_hash) = 32),
    window_start_ms BIGINT NOT NULL,
    attempts INTEGER NOT NULL CHECK (attempts > 0),
    PRIMARY KEY (bucket_hash, window_start_ms)
);
CREATE INDEX IF NOT EXISTS eye_oauth_rate_limits_window_idx
    ON eye_oauth_rate_limits(window_start_ms);

-- Supabase's previous PostgREST schema used hex CHAR(64) hashes for these two
-- tables. Convert them once to BYTEA so the direct SQLx runtime has one binary
-- hash representation regardless of whether the legacy migrations ran first.
DO $$
BEGIN
    IF (SELECT data_type FROM information_schema.columns
        WHERE table_schema=current_schema() AND table_name='eye_refresh_tokens' AND column_name='token_hash') = 'character' THEN
        ALTER TABLE eye_refresh_tokens ALTER COLUMN token_hash TYPE BYTEA USING decode(trim(token_hash), 'hex');
    END IF;
    IF (SELECT data_type FROM information_schema.columns
        WHERE table_schema=current_schema() AND table_name='eye_oauth_rate_limits' AND column_name='bucket_hash') = 'character' THEN
        ALTER TABLE eye_oauth_rate_limits ALTER COLUMN bucket_hash TYPE BYTEA USING decode(trim(bucket_hash), 'hex');
    END IF;
END $$;
