-- Bind OAuth transactions to the SHA-256 hash of the HttpOnly browser cookie.
-- Existing transactions are short-lived (10 minutes); invalidate them during this
-- one-time schema transition rather than assigning an unverifiable cookie binding.
BEGIN;
DELETE FROM oauth_transactions;
ALTER TABLE oauth_transactions
    ADD COLUMN cookie_hash BYTEA NOT NULL
        CHECK (octet_length(cookie_hash) = 32);
CREATE UNIQUE INDEX oauth_transactions_cookie_hash_uidx
    ON oauth_transactions (cookie_hash);
COMMIT;
