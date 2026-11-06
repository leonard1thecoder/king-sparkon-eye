//! PostgreSQL-backed billing ownership and concurrency tests.
//!
//! Set EYE_TEST_DATABASE_URL to a dedicated loopback database whose name contains "test".
//! The test refuses non-loopback hosts and production-looking database names.

use std::time::Duration;

use king_sparkon_eye::{
    billing::stripe::StripeMode,
    persistence::{
        MIGRATOR,
        billing_accounts::{BillingAccountStore, BillingAccountStoreError},
    },
};
use sqlx::{PgPool, postgres::PgPoolOptions};

async fn test_pool() -> Option<PgPool> {
    let database_url = match std::env::var("EYE_TEST_DATABASE_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!(
                "skipping billing PostgreSQL integration test: EYE_TEST_DATABASE_URL is unset"
            );
            return None;
        }
    };
    let parsed = reqwest::Url::parse(&database_url)
        .expect("EYE_TEST_DATABASE_URL must be a valid PostgreSQL URL");
    let loopback = matches!(parsed.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    let database_name = parsed.path().trim_matches('/').to_ascii_lowercase();
    assert!(
        loopback && database_name.contains("test"),
        "billing integration tests require a loopback PostgreSQL database with 'test' in its name"
    );
    let pool = PgPoolOptions::new()
        .max_connections(12)
        .acquire_timeout(Duration::from_secs(5))
        .connect(&database_url)
        .await
        .expect("connect to dedicated billing test database");
    MIGRATOR.run(&pool).await.expect("apply Eye migrations");
    Some(pool)
}

fn new_owner() -> String {
    format!("usr_{}", uuid::Uuid::new_v4().simple())
}

#[tokio::test]
async fn concurrent_mapping_is_unique_and_accounts_and_modes_are_isolated() {
    let Some(pool) = test_pool().await else {
        return;
    };
    let store = BillingAccountStore::new(pool.clone());
    let owner_a = new_owner();
    let owner_b = new_owner();
    let customer_a = format!("cus_{}", uuid::Uuid::new_v4().simple());
    let customer_b = format!("cus_{}", uuid::Uuid::new_v4().simple());
    let customer_a_live = format!("cus_{}", uuid::Uuid::new_v4().simple());

    let mut tasks = Vec::new();
    for _ in 0..10 {
        let store = store.clone();
        let owner = owner_a.clone();
        let customer = customer_a.clone();
        tasks.push(tokio::spawn(async move {
            store
                .insert_customer_mapping(&owner, StripeMode::Test, &customer)
                .await
        }));
    }
    for task in tasks {
        assert_eq!(
            task.await.expect("mapping task"),
            Ok(customer_a.clone()),
            "concurrent inserts for the same owner/mode/customer must converge"
        );
    }

    let count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM eye_billing_accounts WHERE eye_user_id = $1 AND stripe_mode = 'test'",
    )
    .bind(&owner_a)
    .fetch_one(&pool)
    .await
    .expect("count persisted mapping");
    assert_eq!(count, 1, "one durable mapping must exist for an owner/mode");

    assert_eq!(
        store
            .insert_customer_mapping(
                &owner_a,
                StripeMode::Test,
                &format!("cus_{}", uuid::Uuid::new_v4().simple()),
            )
            .await,
        Err(BillingAccountStoreError::MappingConflict),
        "a second Customer for the same owner/mode must fail closed"
    );

    assert_eq!(
        store
            .insert_customer_mapping(&owner_b, StripeMode::Test, &customer_b)
            .await
            .expect("second account mapping"),
        customer_b
    );
    assert_eq!(
        store
            .insert_customer_mapping(&owner_a, StripeMode::Live, &customer_a_live)
            .await
            .expect("separate mode mapping"),
        customer_a_live
    );

    assert_eq!(
        store
            .customer_for(&owner_a, StripeMode::Test)
            .await
            .unwrap(),
        Some(customer_a.clone())
    );
    assert_eq!(
        store
            .customer_for(&owner_a, StripeMode::Live)
            .await
            .unwrap(),
        Some(customer_a_live)
    );
    assert_eq!(
        store
            .customer_for(&owner_b, StripeMode::Test)
            .await
            .unwrap(),
        Some(customer_b)
    );
    assert_eq!(
        store
            .customer_for(&owner_b, StripeMode::Live)
            .await
            .unwrap(),
        None,
        "one Eye account cannot resolve another account or mode's Customer"
    );

    sqlx::query("DELETE FROM eye_billing_accounts WHERE eye_user_id = ANY($1)")
        .bind(vec![owner_a, owner_b])
        .execute(&pool)
        .await
        .expect("clean up test billing accounts");
}
