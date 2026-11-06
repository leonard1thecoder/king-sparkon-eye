//! Durable OAuth transaction state and one-time handoff codes (issue #112).
//!
//! Callers pass hashes, never raw codes or nonces. Consumption is a single
//! atomic UPDATE, so replay is rejected across processes and restarts.
//! Expiry is computed by the database clock to avoid application clock skew.

use sqlx::PgPool;

/// A stored OAuth transaction, returned exactly once when consumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsumedTransaction {
    pub nonce_hash: Vec<u8>,
    pub nonce_enc: Vec<u8>,
    pub pkce_verifier_enc: Vec<u8>,
    pub callback_url: String,
    /// False when the cookie was found but state mismatched or the transaction expired.
    /// The record is consumed regardless, preventing callback retries.
    pub state_valid: bool,
}

/// Input for creating a durable OAuth transaction.
pub struct NewTransaction<'a> {
    pub cookie_hash: &'a [u8; 32],
    pub state_hash: &'a [u8; 32],
    pub nonce_hash: &'a [u8; 32],
    pub nonce_enc: &'a [u8],
    pub pkce_verifier_enc: &'a [u8],
    pub callback_url: &'a str,
    pub ttl_secs: i64,
}

type ConsumedTransactionRow = (Vec<u8>, Vec<u8>, Vec<u8>, String, bool);

/// Store an OAuth transaction with a bounded expiry.
pub async fn insert_transaction(
    pool: &PgPool,
    input: NewTransaction<'_>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO oauth_transactions
            (cookie_hash, state_hash, nonce_hash, nonce_enc, pkce_verifier_enc, callback_url, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, now() + make_interval(secs => $7))",
    )
    .bind(&input.cookie_hash[..])
    .bind(&input.state_hash[..])
    .bind(&input.nonce_hash[..])
    .bind(input.nonce_enc)
    .bind(input.pkce_verifier_enc)
    .bind(input.callback_url)
    .bind(input.ttl_secs as f64)
    .execute(pool)
    .await?;
    Ok(())
}

/// Atomically consume an OAuth transaction. Returns `None` when the state is
/// unknown, already consumed, or expired. In every case the row is never reused.
pub async fn consume_transaction(
    pool: &PgPool,
    cookie_hash: &[u8; 32],
    state_hash: &[u8; 32],
) -> Result<Option<ConsumedTransaction>, sqlx::Error> {
    // Consume by the HttpOnly cookie binding, not by caller-controlled state alone.
    // A found transaction is burned even when state mismatches or it has expired.
    let row: Option<ConsumedTransactionRow> = sqlx::query_as(
        "UPDATE oauth_transactions
            SET consumed_at = now()
          WHERE cookie_hash = $1
            AND consumed_at IS NULL
         RETURNING nonce_hash, nonce_enc, pkce_verifier_enc, callback_url,
                   (state_hash = $2 AND expires_at > now())",
    )
    .bind(&cookie_hash[..])
    .bind(&state_hash[..])
    .fetch_optional(pool)
    .await?;

    Ok(row.map(
        |(nonce_hash, nonce_enc, pkce_verifier_enc, callback_url, state_valid)| {
            ConsumedTransaction {
                nonce_hash,
                nonce_enc,
                pkce_verifier_enc,
                callback_url,
                state_valid,
            }
        },
    ))
}

/// Store a one-time handoff code hash bound to a user and callback URL.
pub async fn insert_handoff(
    pool: &PgPool,
    code_hash: &[u8; 32],
    user_id: &str,
    callback_url: &str,
    ttl_secs: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO oauth_handoffs (code_hash, user_id, callback_url, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(secs => $4))",
    )
    .bind(&code_hash[..])
    .bind(user_id)
    .bind(callback_url)
    .bind(ttl_secs as f64)
    .execute(pool)
    .await?;
    Ok(())
}

/// Atomically consume a handoff code. Succeeds at most once per code.
/// A callback mismatch or expiry still consumes the code, preventing retries.
pub async fn consume_handoff(
    pool: &PgPool,
    code_hash: &[u8; 32],
    callback_url: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String, bool)> = sqlx::query_as(
        "UPDATE oauth_handoffs
            SET consumed_at = now()
          WHERE code_hash = $1
            AND consumed_at IS NULL
         RETURNING user_id, (callback_url = $2 AND expires_at > now())",
    )
    .bind(&code_hash[..])
    .bind(callback_url)
    .fetch_optional(pool)
    .await?;
    Ok(row.and_then(|(user_id, valid)| valid.then_some(user_id)))
}
