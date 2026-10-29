//! Integration tests for Eye's PostgreSQL OAuth identity state (issue #112).
//!
//! Requires `EYE_TEST_DATABASE_URL`, pointing at a disposable PostgreSQL database.
//! If it is unset, each test prints a notice and returns. CI must set it so the
//! checks are actually run; a skipped test is not a passed test.

use king_sparkon_eye::persistence::identity::LinkOutcome;
use king_sparkon_eye::persistence::{connect, identity, migrate, oauth};
use sqlx::PgPool;
use std::sync::Arc;
use uuid::Uuid;

async fn pool() -> Option<PgPool> {
    let url = match std::env::var("EYE_TEST_DATABASE_URL") {
        Ok(u) if !u.trim().is_empty() => u,
        _ => {
            eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; persistence checks not run");
            return None;
        }
    };
    let pool = connect(&url).await.expect("connect to test database");
    migrate(&pool).await.expect("apply migrations");
    Some(pool)
}

fn hash(tag: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut out = [0u8; 32];
    out.copy_from_slice(&Sha256::digest(format!("{tag}:{}", Uuid::new_v4())));
    out
}

fn user() -> String {
    format!("usr_{}", Uuid::new_v4().simple())
}

fn subject() -> String {
    format!("google-sub-{}", Uuid::new_v4().simple())
}

#[tokio::test]
async fn migrations_are_idempotent() {
    let Some(pool) = pool().await else { return };
    migrate(&pool).await.expect("second migrate run is a no-op");
}

#[tokio::test]
async fn subject_links_once_and_is_idempotent_for_same_user() {
    let Some(pool) = pool().await else { return };
    let (s, u) = (subject(), user());
    assert_eq!(
        identity::link_google_identity(&pool, &s, &u, true)
            .await
            .unwrap(),
        LinkOutcome::Linked
    );
    assert_eq!(
        identity::link_google_identity(&pool, &s, &u, true)
            .await
            .unwrap(),
        LinkOutcome::AlreadyLinked
    );
    assert_eq!(
        identity::find_google_user(&pool, &s).await.unwrap(),
        Some(u)
    );
}

#[tokio::test]
async fn subject_cannot_be_reassigned_to_another_user() {
    let Some(pool) = pool().await else { return };
    let (s, u1, u2) = (subject(), user(), user());
    identity::link_google_identity(&pool, &s, &u1, true)
        .await
        .unwrap();
    assert_eq!(
        identity::link_google_identity(&pool, &s, &u2, true)
            .await
            .unwrap(),
        LinkOutcome::Conflict
    );
    assert_eq!(
        identity::find_google_user(&pool, &s).await.unwrap(),
        Some(u1),
        "original owner kept"
    );
}

#[tokio::test]
async fn user_cannot_gain_a_second_google_subject() {
    let Some(pool) = pool().await else { return };
    let (s1, s2, u) = (subject(), subject(), user());
    identity::link_google_identity(&pool, &s1, &u, true)
        .await
        .unwrap();
    assert_eq!(
        identity::link_google_identity(&pool, &s2, &u, true)
            .await
            .unwrap(),
        LinkOutcome::Conflict
    );
    assert_eq!(
        identity::find_google_user(&pool, &s2).await.unwrap(),
        None,
        "second subject not linked"
    );
}

#[tokio::test]
async fn concurrent_link_of_same_subject_has_one_winner() {
    let Some(pool) = pool().await else { return };
    let pool = Arc::new(pool);
    let (s, u1, u2) = (subject(), user(), user());
    let (p1, p2) = (pool.clone(), pool.clone());
    let (s1, s2) = (s.clone(), s.clone());
    let (a, b) = tokio::join!(
        async move {
            identity::link_google_identity(&p1, &s1, &u1, true)
                .await
                .unwrap()
        },
        async move {
            identity::link_google_identity(&p2, &s2, &u2, true)
                .await
                .unwrap()
        },
    );
    let linked = [a, b].iter().filter(|o| **o == LinkOutcome::Linked).count();
    assert_eq!(linked, 1, "exactly one concurrent link wins: {a:?} / {b:?}");
}

#[tokio::test]
async fn handoff_is_single_use_and_bound_to_callback() {
    let Some(pool) = pool().await else { return };
    let (code, u) = (hash("handoff"), user());
    let cb = "https://eye.example.test/auth/callback";
    oauth::insert_handoff(&pool, &code, &u, cb, 60)
        .await
        .unwrap();

    assert_eq!(
        oauth::consume_handoff(&pool, &code, "https://evil.example.test/x")
            .await
            .unwrap(),
        None,
        "wrong callback rejected"
    );
    assert_eq!(
        oauth::consume_handoff(&pool, &code, cb).await.unwrap(),
        Some(u.clone()),
        "first exchange succeeds"
    );
    assert_eq!(
        oauth::consume_handoff(&pool, &code, cb).await.unwrap(),
        None,
        "replay rejected"
    );
}

#[tokio::test]
async fn concurrent_handoff_exchange_succeeds_once() {
    let Some(pool) = pool().await else { return };
    let pool = Arc::new(pool);
    let (code, u) = (hash("race"), user());
    let cb = "https://eye.example.test/auth/callback";
    oauth::insert_handoff(&pool, &code, &u, cb, 60)
        .await
        .unwrap();
    let (p1, p2) = (pool.clone(), pool.clone());
    let (a, b) = tokio::join!(
        async move { oauth::consume_handoff(&p1, &code, cb).await.unwrap() },
        async move { oauth::consume_handoff(&p2, &code, cb).await.unwrap() },
    );
    let wins = [a, b].iter().filter(|r| r.is_some()).count();
    assert_eq!(wins, 1, "exactly one concurrent exchange wins");
}

#[tokio::test]
async fn expired_handoff_is_rejected() {
    let Some(pool) = pool().await else { return };
    let (code, u) = (hash("expired"), user());
    let cb = "https://eye.example.test/auth/callback";
    oauth::insert_handoff(&pool, &code, &u, cb, 0)
        .await
        .unwrap();
    assert_eq!(
        oauth::consume_handoff(&pool, &code, cb).await.unwrap(),
        None
    );
}

#[tokio::test]
async fn transaction_is_consumed_once_and_survives_reconnect() {
    let Some(pool) = pool().await else { return };
    let state = hash("state");
    let (nonce, verifier) = (hash("nonce"), b"encrypted-verifier".to_vec());
    let cb = "https://eye.example.test/auth/callback";
    oauth::insert_transaction(&pool, &state, &nonce, &verifier, cb, 600)
        .await
        .unwrap();

    // Simulate a restart: a fresh pool must see the same durable state.
    drop(pool);
    let url = std::env::var("EYE_TEST_DATABASE_URL").unwrap();
    let fresh = connect(&url).await.unwrap();

    let got = oauth::consume_transaction(&fresh, &state)
        .await
        .unwrap()
        .expect("present after restart");
    assert_eq!(got.nonce_hash, nonce.to_vec());
    assert_eq!(got.pkce_verifier_enc, verifier);
    assert_eq!(got.callback_url, cb);
    assert_eq!(
        oauth::consume_transaction(&fresh, &state).await.unwrap(),
        None,
        "replay rejected"
    );
}
