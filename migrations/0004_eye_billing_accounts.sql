-- One self-service billing account per registered Eye user and Stripe mode.
-- No Tracker tenant or external business identity participates in account ownership.

CREATE TABLE IF NOT EXISTS eye_billing_accounts (
    id BIGSERIAL PRIMARY KEY,
    eye_user_id TEXT NOT NULL
        CHECK (eye_user_id ~ '^usr_[0-9a-f]{32}$'),
    stripe_mode TEXT NOT NULL CHECK (stripe_mode IN ('test', 'live')),
    stripe_customer_id TEXT NOT NULL
        CHECK (stripe_customer_id ~ '^cus_[A-Za-z0-9_]{1,250}$'),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT uq_eye_billing_account_owner_mode UNIQUE (eye_user_id, stripe_mode),
    CONSTRAINT uq_eye_billing_customer_mode UNIQUE (stripe_mode, stripe_customer_id)
);

CREATE INDEX IF NOT EXISTS ix_eye_billing_accounts_owner
    ON eye_billing_accounts (eye_user_id);
