//! Durable provider identity links (issue #112).
//!
//! A Google subject links to exactly one Eye user, and a user holds at most
//! one Google subject. Conflicts are rejected, never reassigned.

use sqlx::PgPool;

/// Outcome of linking a provider subject to an Eye user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkOutcome {
    /// A new link was created.
    Linked,
    /// The subject was already linked to this same user. Idempotent success.
    AlreadyLinked,
    /// The subject belongs to another user, or the user already has a different subject.
    Conflict,
}

/// Link a Google subject to an Eye user.
///
/// Email-based linking is decided by the caller, who must require
/// `email_verified == true` before calling with a matched email. This function
/// records the decision but never performs email matching itself.
pub async fn link_google_identity(
    pool: &PgPool,
    provider_subject: &str,
    user_id: &str,
    email_verified: bool,
) -> Result<LinkOutcome, sqlx::Error> {
    // ON CONFLICT DO NOTHING absorbs both unique violations (subject, and the
    // user's existing provider link). We then read back to classify the outcome.
    let inserted = sqlx::query(
        "INSERT INTO identity_links (provider, provider_subject, user_id, email_verified)
         VALUES ('google', $1, $2, $3)
         ON CONFLICT DO NOTHING",
    )
    .bind(provider_subject)
    .bind(user_id)
    .bind(email_verified)
    .execute(pool)
    .await?
    .rows_affected();

    if inserted == 1 {
        return Ok(LinkOutcome::Linked);
    }

    let existing: Option<(String,)> = sqlx::query_as(
        "SELECT user_id FROM identity_links
         WHERE provider = 'google' AND provider_subject = $1",
    )
    .bind(provider_subject)
    .fetch_optional(pool)
    .await?;

    Ok(match existing {
        Some((owner,)) if owner == user_id => LinkOutcome::AlreadyLinked,
        // Subject held by another user.
        Some(_) => LinkOutcome::Conflict,
        // Subject not linked, so the insert must have failed on the user's
        // existing provider link to a different subject.
        None => LinkOutcome::Conflict,
    })
}

/// Look up the Eye user linked to a Google subject.
pub async fn find_google_user(
    pool: &PgPool,
    provider_subject: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row: Option<(String,)> = sqlx::query_as(
        "SELECT user_id FROM identity_links
         WHERE provider = 'google' AND provider_subject = $1",
    )
    .bind(provider_subject)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(u,)| u))
}
