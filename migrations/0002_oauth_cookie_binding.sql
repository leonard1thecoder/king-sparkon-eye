-- Bind each OAuth transaction to the hash of its HttpOnly browser cookie.
-- Existing transactions are short-lived (10 minutes); invalidate them during this
-- schema transition rather than inventing a cookie binding for old rows.
BEGIN;
DELETE FROM oauth_transactions;
ALTER TABLE oauth_transactions
    ADD COLUMN cookie_hash bytea NOT NULL
        CHECK (octet_length(cookie_hash) = 32);
CREATE UNIQUE INDEX oauth_transactions_cookie_hash_uidx
    ON oauth_transactions (cookie_hash);
COMMIT;
