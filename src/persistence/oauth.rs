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
    pub pkce_verifier_enc: Vec<u8>,
    pub callback_url: String,
}

/// Store an OAuth transaction that expires after `ttl_secs`.
pub async fn insert_transaction(
    pool: &PgPool,
    state_hash: &[u8; 32],
    nonce_hash: &[u8; 32],
    pkce_verifier_enc: &[u8],
    callback_url: &str,
    ttl_secs: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO oauth_transactions
            (state_hash, nonce_hash, pkce_verifier_enc, callback_url, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5))",
    )
    .bind(&state_hash[..])
    .bind(&nonce_hash[..])
    .bind(pkce_verifier_enc)
    .bind(callback_url)
    .bind(ttl_secs as f64)
    .execute(pool)
    .await?;
    Ok(())
}

/// Atomically consume an OAuth transaction. Returns `None` when the state is
/// unknown, already consumed, or expired. In every case the row is never reused.
pub async fn consume_transaction(
    pool: &PgPool,
    state_hash: &[u8; 32],
) -> Result<Option<ConsumedTransaction>, sqlx::Error> {
    let row: Option<(Vec<u8>, Vec<u8>, String)> = sqlx::query_as(
        "UPDATE oauth_transactions
            SET consumed_at = now()
          WHERE state_hash = $1
            AND consumed_at IS NULL
            AND expires_at > now()
         RETURNING nonce_hash, pkce_verifier_enc, callback_url",
    )
    .bind(&state_hash[..])
    .fetch_optional(pool)
    .await?;

    Ok(row.map(
        |(nonce_hash, pkce_verifier_enc, callback_url)| ConsumedTransaction {
            nonce_hash,
            pkce_verifier_enc,
            callback_url,
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
/// Returns the bound user ID only when the callback matches and the code is live.
/// A callback mismatch does not consume the code.
pub async fn consume_handoff(
    pool: &PgPool,
    code_hash: &[u8; 32],
    callback_url: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(
        "UPDATE oauth_handoffs
            SET consumed_at = now()
          WHERE code_hash = $1
            AND callback_url = $2
            AND consumed_at IS NULL
            AND expires_at > now()
         RETURNING user_id",
    )
    .bind(&code_hash[..])
    .bind(callback_url)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(u,)| u))
}
