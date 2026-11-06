//! PostgreSQL integration tests for the durable billing usage ledger (#148).
//! Requires EYE_TEST_DATABASE_URL pointing at a disposable Eye PostgreSQL database.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use king_sparkon_eye::{
    billing::{
        AllowancePeriod, MeterId,
        ledger::{AppendBody, AppendOutcome, AppendRequest, EventKey, TenantId, UsageSource},
    },
    persistence::{connect, migrate, usage_ledger::{UsageLedgerStore, UsageStoreError}},
};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use std::str::FromStr;
use uuid::Uuid;

struct Fixture {
    options: PgConnectOptions,
}

async fn fixture() -> Option<Fixture> {
    let url = match std::env::var("EYE_TEST_DATABASE_URL") {
        Ok(value) if !value.trim().is_empty() => value,
        _ => {
            eprintln!("SKIPPED: EYE_TEST_DATABASE_URL not set; billing usage persistence tests not run");
            return None;
        }
    };
    let schema = format!("billing_usage_test_{}", Uuid::new_v4().simple());
    let admin = connect(&url).await.expect("connect to Eye test PostgreSQL");
    sqlx::query(sqlx::AssertSqlSafe(format!("CREATE SCHEMA \"{schema}\"")))
        .execute(&admin)
        .await
        .expect("create per-test schema");
    admin.close().await;
    let options = PgConnectOptions::from_str(&url)
        .expect("EYE_TEST_DATABASE_URL is a valid PostgreSQL URL")
        .options([("search_path", schema.as_str())]);
    Some(Fixture { options })
}

impl Fixture {
    async fn pool(&self) -> PgPool {
        PgPoolOptions::new()
            .max_connections(8)
            .connect_with(self.options.clone())
            .await
            .expect("connect to the per-test schema")
    }

    async fn store(&self) -> UsageLedgerStore {
        let pool = self.pool().await;
        migrate(&pool).await.expect("apply SQLx migrations");
        UsageLedgerStore::new(pool)
    }
}

fn instant(day: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, day, 0, 0, 0)
        .single()
        .expect("valid UTC instant")
}

fn request(
    tenant: &str,
    key: &str,
    source: UsageSource,
    occurred_at: DateTime<Utc>,
    body: AppendBody,
) -> AppendRequest {
    AppendRequest {
        tenant: TenantId::new(tenant).expect("valid tenant"),
        key: EventKey::new(key).expect("valid event key"),
        source,
        occurred_at,
        body,
    }
}

#[tokio::test]
async fn append_is_durable_idempotent_and_creates_reporting_intent_atomically() {
    let Some(fx) = fixture().await else { return };
    let store = fx.store().await;
    let item = request(
        "tenant_a",
        "enrollment-001",
        UsageSource::ConfirmedEnrollment,
        instant(3),
        AppendBody::Flow { quantity: 1 },
    );

    assert!(matches!(
        store.append(item.clone()).await.unwrap(),
        AppendOutcome::Appended { .. }
    ));
    assert!(matches!(
        store.append(item).await.unwrap(),
        AppendOutcome::Duplicate { .. }
    ));

    let period = AllowancePeriod::containing(instant(3));
    assert_eq!(
        store
            .flow_total("tenant_a", &period, MeterId::Enrollments)
            .await
            .unwrap(),
        1
    );

    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM eye_billing_usage_outbox")
        .fetch_one(&fx.pool().await)
        .await
        .unwrap();
    assert_eq!(count, 1);
}

#[tokio::test]
async fn reporting_outbox_uses_leases_retries_and_acknowledgement() {
    let Some(fx) = fixture().await else { return };
    let store = fx.store().await;
    store.append(request(
        "tenant_a",
        "reportable-identification",
        UsageSource::EligibleIdentificationCompleted,
        instant(3),
        AppendBody::Flow { quantity: 1 },
    )).await.unwrap();

    let now = instant(3) + chrono::Duration::seconds(1);
    let claimed = store.claim_due(now, chrono::Duration::minutes(2), 10).await.unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].tenant_id, "tenant_a");
    assert_eq!(claimed[0].meter, MeterId::IdentificationRequests);
    assert_eq!(claimed[0].attempts, 1);

    assert!(store.schedule_retry(
        claimed[0].id,
        claimed[0].attempts,
        now,
        now + chrono::Duration::minutes(5),
        "stripe_unavailable",
        3,
    ).await.unwrap());

    assert!(store.claim_due(now + chrono::Duration::minutes(4), chrono::Duration::minutes(2), 10).await.unwrap().is_empty());
    let retry = store.claim_due(now + chrono::Duration::minutes(6), chrono::Duration::minutes(2), 10).await.unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(retry[0].attempts, 2);

    assert!(store.mark_reported(retry[0].id, retry[0].attempts, now + chrono::Duration::minutes(6)).await.unwrap());
    assert!(!store.mark_reported(retry[0].id, retry[0].attempts, now + chrono::Duration::minutes(7)).await.unwrap());
    assert!(store.claim_due(now + chrono::Duration::minutes(8), chrono::Duration::minutes(2), 10).await.unwrap().is_empty());
}

#[tokio::test]
async fn conflicting_event_key_and_cross_tenant_reversal_are_rejected() {
    let Some(fx) = fixture().await else { return };
    let store = fx.store().await;
    let original = request(
        "tenant_a",
        "identification-001",
        UsageSource::EligibleIdentificationCompleted,
        instant(4),
        AppendBody::Flow { quantity: 1 },
    );
    store.append(original).await.unwrap();

    let conflict = request(
        "tenant_a",
        "identification-001",
        UsageSource::EligibleIdentificationCompleted,
        instant(4),
        AppendBody::Flow { quantity: 2 },
    );
    assert_eq!(
        store.append(conflict).await.unwrap_err(),
        UsageStoreError::KeyConflict
    );

    let foreign_reversal = request(
        "tenant_b",
        "reversal-foreign",
        UsageSource::EligibleIdentificationCompleted,
        instant(5),
        AppendBody::Reversal {
            reverses: EventKey::new("identification-001").unwrap(),
        },
    );
    assert_eq!(
        store.append(foreign_reversal).await.unwrap_err(),
        UsageStoreError::ReversalTargetMissing
    );
}

#[tokio::test]
async fn reversal_is_append_only_idempotent_and_booked_to_original_period() {
    let Some(fx) = fixture().await else { return };
    let store = fx.store().await;
    let original = request(
        "tenant_a",
        "enrollment-reverse-me",
        UsageSource::ConfirmedEnrollment,
        Utc.with_ymd_and_hms(2026, 10, 31, 23, 59, 0).single().unwrap(),
        AppendBody::Flow { quantity: 3 },
    );
    store.append(original).await.unwrap();

    let reversal = request(
        "tenant_a",
        "enrollment-reversal",
        UsageSource::ConfirmedEnrollment,
        instant(2),
        AppendBody::Reversal {
            reverses: EventKey::new("enrollment-reverse-me").unwrap(),
        },
    );
    assert!(matches!(
        store.append(reversal.clone()).await.unwrap(),
        AppendOutcome::Appended { .. }
    ));
    assert!(matches!(
        store.append(reversal).await.unwrap(),
        AppendOutcome::Duplicate { .. }
    ));

    let october = AllowancePeriod::containing(
        Utc.with_ymd_and_hms(2026, 10, 31, 23, 59, 0).single().unwrap(),
    );
    let november = AllowancePeriod::containing(instant(2));
    assert_eq!(
        store.flow_total("tenant_a", &october, MeterId::Enrollments).await.unwrap(),
        0
    );
    assert_eq!(
        store.flow_total("tenant_a", &november, MeterId::Enrollments).await.unwrap(),
        0
    );

    let second_reversal = request(
        "tenant_a",
        "enrollment-reversal-second",
        UsageSource::ConfirmedEnrollment,
        instant(3),
        AppendBody::Reversal {
            reverses: EventKey::new("enrollment-reverse-me").unwrap(),
        },
    );
    assert_eq!(
        store.append(second_reversal).await.unwrap_err(),
        UsageStoreError::AlreadyReversed
    );
}

#[tokio::test]
async fn daily_level_samples_use_latest_sample_and_round_monthly_average_up() {
    let Some(fx) = fixture().await else { return };
    let store = fx.store().await;
    let period = AllowancePeriod::containing(instant(1));
    let first = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
    let second = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();

    for item in [
        request("tenant_a", "template-day-1-old", UsageSource::TemplateSample, instant(1), AppendBody::Level { units: 10 }),
        request("tenant_a", "template-day-1-latest", UsageSource::TemplateSample, instant(1), AppendBody::Level { units: 12 }),
        request("tenant_a", "template-day-2", UsageSource::TemplateSample, instant(2), AppendBody::Level { units: 21 }),
    ] {
        store.append(item).await.unwrap();
    }

    assert_eq!(
        store.level_billable_quantity("tenant_a", &period, MeterId::ActiveTemplates, first, second).await.unwrap(),
        17
    );
}

#[tokio::test]
async fn photo_storage_average_uses_gibibytes_and_requires_every_day() {
    let Some(fx) = fixture().await else { return };
    let store = fx.store().await;
    let period = AllowancePeriod::containing(instant(1));
    let first = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
    let second = NaiveDate::from_ymd_opt(2026, 11, 2).unwrap();
    let gib = 1024_u64 * 1024 * 1024;

    store.append(request(
        "tenant_a",
        "photo-day-1",
        UsageSource::PhotoStorageSample,
        instant(1),
        AppendBody::Level { units: gib },
    )).await.unwrap();

    assert_eq!(
        store.level_billable_quantity("tenant_a", &period, MeterId::RetainedPhotoStorageGb, first, second).await.unwrap_err(),
        UsageStoreError::MissingLevelSample
    );

    store.append(request(
        "tenant_a",
        "photo-day-2",
        UsageSource::PhotoStorageSample,
        instant(2),
        AppendBody::Level { units: gib * 2 },
    )).await.unwrap();

    assert_eq!(
        store.level_billable_quantity("tenant_a", &period, MeterId::RetainedPhotoStorageGb, first, second).await.unwrap(),
        2
    );
}
