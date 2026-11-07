//! PostgreSQL-backed billing ownership and concurrency tests.
//!
//! Set EYE_TEST_DATABASE_URL to a disposable loopback database or explicitly opted-in Supabase test project.

use king_sparkon_eye::{
    billing::stripe::StripeMode,
    persistence::{
        self,
        billing_accounts::{BillingAccountStore, BillingAccountStoreError},
    },
};
use sqlx::PgPool;

fn assert_disposable_test_database_url(url: &str) {
    let parsed =
        reqwest::Url::parse(url).expect("EYE_TEST_DATABASE_URL must be a valid PostgreSQL URL");
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let username = parsed.username().to_ascii_lowercase();
    let database_name = parsed.path().trim_matches('/').to_ascii_lowercase();
    let loopback = matches!(host.as_str(), "localhost" | "127.0.0.1" | "[::1]");

    if loopback {
        assert!(
            database_name.contains("test"),
            "test database name must contain 'test'"
        );
        return;
    }

    assert_eq!(
        std::env::var("EYE_TEST_DATABASE_ALLOW_SUPABASE").as_deref(),
        Ok("1"),
        "remote PostgreSQL is blocked; explicitly opt in only for a dedicated Supabase test project"
    );
    assert_ne!(
        parsed.port(),
        Some(6543),
        "Supabase transaction pooler is not supported by SQLx prepared statements; use the session pooler on port 5432"
    );

    let direct_project_ref = host
        .strip_prefix("db.")
        .and_then(|value| value.strip_suffix(".supabase.co"));
    let pooler_project_ref = if host.ends_with(".pooler.supabase.com") {
        username.strip_prefix("postgres.")
    } else {
        None
    };
    let is_direct_project = direct_project_ref
        .is_some_and(|project_ref| !project_ref.is_empty() && username == "postgres");
    let is_session_pooler = pooler_project_ref.is_some_and(|project_ref| !project_ref.is_empty());
    assert!(
        is_direct_project || is_session_pooler,
        "remote test URL must be a Supabase direct host (db.<project-ref>.supabase.co) or session pooler with postgres.<project-ref> user"
    );
}

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
    assert_disposable_test_database_url(&database_url);
    let pool = persistence::connect(&database_url)
        .await
        .expect("connect to dedicated billing test database");
    persistence::migrate(&pool)
        .await
        .expect("apply Eye migrations");
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
