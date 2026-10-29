-- Eye-owned OAuth identity state (issue #112).
-- Tables are separate from Tracker/Spring Boot auth tables by design.
-- Only hashes of one-time codes are stored. The PKCE verifier is stored encrypted
-- by the application (key from deployment secret store) and is deleted on use.

CREATE TABLE identity_links (
    provider          text        NOT NULL CHECK (provider = 'google'),
    provider_subject  text        NOT NULL CHECK (char_length(provider_subject) BETWEEN 1 AND 255),
    user_id           text        NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 128),
    email_verified    boolean     NOT NULL,
    created_at        timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, provider_subject),
    -- One provider identity per Eye user: a user cannot silently gain a second subject.
    UNIQUE (provider, user_id)
);

CREATE TABLE oauth_transactions (
    state_hash         bytea       PRIMARY KEY CHECK (octet_length(state_hash) = 32),
    nonce_hash         bytea       NOT NULL CHECK (octet_length(nonce_hash) = 32),
    pkce_verifier_enc  bytea       NOT NULL,
    callback_url       text        NOT NULL CHECK (char_length(callback_url) BETWEEN 1 AND 2048),
    created_at         timestamptz NOT NULL DEFAULT now(),
    expires_at         timestamptz NOT NULL,
    consumed_at        timestamptz NULL
);
CREATE INDEX oauth_transactions_expires_at_idx ON oauth_transactions (expires_at);

CREATE TABLE oauth_handoffs (
    code_hash     bytea       PRIMARY KEY CHECK (octet_length(code_hash) = 32),
    user_id       text        NOT NULL CHECK (char_length(user_id) BETWEEN 1 AND 128),
    callback_url  text        NOT NULL CHECK (char_length(callback_url) BETWEEN 1 AND 2048),
    created_at    timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz NOT NULL,
    consumed_at   timestamptz NULL
);
CREATE INDEX oauth_handoffs_expires_at_idx ON oauth_handoffs (expires_at);
