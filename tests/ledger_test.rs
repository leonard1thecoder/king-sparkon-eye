use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use king_sparkon_eye::billing::ledger::{
    AppendBody, AppendOutcome, AppendRequest, BYTES_PER_GIB, EventKey, LedgerError, LedgerModel,
    Reconciliation, TenantId, UsageSource, reconcile,
};
use king_sparkon_eye::billing::{
    AllowancePeriod, BillingInterval, MeterId, MeterOutcome, PerMeter, PlanCatalog, PlanId,
    assess_usage,
};

const CANDIDATE: &str = include_str!("../config/billing/plan-catalog.candidate.json");

fn utc(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, s)
        .single()
        .expect("valid instant")
}

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).expect("valid date")
}

fn tenant(raw: &str) -> TenantId {
    TenantId::new(raw).expect("valid tenant")
}

fn key(raw: &str) -> EventKey {
    EventKey::new(raw).expect("valid key")
}

fn flow(
    tenant_id: &str,
    event: &str,
    source: UsageSource,
    at: DateTime<Utc>,
    quantity: u64,
) -> AppendRequest {
    AppendRequest {
        tenant: tenant(tenant_id),
        key: key(event),
        source,
        occurred_at: at,
        body: AppendBody::Flow { quantity },
    }
}

fn reversal(
    tenant_id: &str,
    event: &str,
    source: UsageSource,
    at: DateTime<Utc>,
    reverses: &str,
) -> AppendRequest {
    AppendRequest {
        tenant: tenant(tenant_id),
        key: key(event),
        source,
        occurred_at: at,
        body: AppendBody::Reversal {
            reverses: key(reverses),
        },
    }
}

fn sample(
    tenant_id: &str,
    event: &str,
    source: UsageSource,
    at: DateTime<Utc>,
    units: u64,
) -> AppendRequest {
    AppendRequest {
        tenant: tenant(tenant_id),
        key: key(event),
        source,
        occurred_at: at,
        body: AppendBody::Level { units },
    }
}

fn period_of(at: DateTime<Utc>) -> AllowancePeriod {
    AllowancePeriod::containing(at)
}

// ---- Identifiers ----

#[test]
fn tenant_and_key_identifiers_are_validated() {
    assert!(TenantId::new("tenant-01_a").is_ok());
    let too_long = "x".repeat(65);
    for bad in ["", "has space", "a/b", too_long.as_str()] {
        assert_eq!(
            TenantId::new(bad),
            Err(LedgerError::InvalidTenant),
            "tenant {bad:?}"
        );
    }
    assert!(EventKey::new("enroll:2026-10-30.001").is_ok());
    let too_long = "x".repeat(129);
    for bad in ["", "bad key", too_long.as_str()] {
        assert_eq!(
            EventKey::new(bad),
            Err(LedgerError::InvalidEventKey),
            "key {bad:?}"
        );
    }
}

// ---- Idempotency and append-only behaviour ----

#[test]
fn replayed_event_is_a_duplicate_and_does_not_double_count() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 10, 0, 0);
    let req = flow("t1", "enroll-1", UsageSource::ConfirmedEnrollment, at, 1);

    assert_eq!(
        ledger.append(req.clone()),
        Ok(AppendOutcome::Appended { seq: 1 })
    );
    assert_eq!(
        ledger.append(req.clone()),
        Ok(AppendOutcome::Duplicate { seq: 1 })
    );
    assert_eq!(ledger.append(req), Ok(AppendOutcome::Duplicate { seq: 1 }));

    let total = ledger
        .flow_total(&tenant("t1"), &period_of(at), MeterId::Enrollments)
        .expect("total");
    assert_eq!(total, 1, "replays must not double-count");
    assert_eq!(ledger.entries().len(), 1);
}

#[test]
fn reusing_a_key_for_a_different_entry_is_a_conflict() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 10, 0, 0);
    ledger
        .append(flow(
            "t1",
            "enroll-1",
            UsageSource::ConfirmedEnrollment,
            at,
            1,
        ))
        .expect("first append");

    let different_time = flow(
        "t1",
        "enroll-1",
        UsageSource::ConfirmedEnrollment,
        utc(2026, 11, 4, 10, 0, 0),
        1,
    );
    assert_eq!(ledger.append(different_time), Err(LedgerError::KeyConflict));

    let different_source = flow(
        "t1",
        "enroll-1",
        UsageSource::EligibleIdentificationCompleted,
        at,
        1,
    );
    assert_eq!(
        ledger.append(different_source),
        Err(LedgerError::KeyConflict)
    );
    assert_eq!(ledger.entries().len(), 1);
}

#[test]
fn keys_are_scoped_per_tenant() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 10, 0, 0);
    assert_eq!(
        ledger.append(flow(
            "t1",
            "shared-key",
            UsageSource::ConfirmedEnrollment,
            at,
            1
        )),
        Ok(AppendOutcome::Appended { seq: 1 })
    );
    assert_eq!(
        ledger.append(flow(
            "t2",
            "shared-key",
            UsageSource::ConfirmedEnrollment,
            at,
            1
        )),
        Ok(AppendOutcome::Appended { seq: 2 })
    );
}

#[test]
fn zero_quantity_flow_is_rejected() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 10, 0, 0);
    assert_eq!(
        ledger.append(flow("t1", "e", UsageSource::ConfirmedEnrollment, at, 0)),
        Err(LedgerError::InvalidQuantity)
    );
}

#[test]
fn source_must_match_the_meter_kind() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 10, 0, 0);
    // A flow body on a level source, and a level body on a flow source, are both mismatches.
    assert_eq!(
        ledger.append(flow("t1", "e1", UsageSource::TemplateSample, at, 1)),
        Err(LedgerError::SourceMeterMismatch)
    );
    assert_eq!(
        ledger.append(sample("t1", "e2", UsageSource::ConfirmedEnrollment, at, 3)),
        Err(LedgerError::SourceMeterMismatch)
    );
}

// ---- Compensation ----

#[test]
fn reversal_cancels_the_original_flow_explicitly() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 10, 0, 0);
    ledger
        .append(flow(
            "t1",
            "enroll-1",
            UsageSource::ConfirmedEnrollment,
            at,
            2,
        ))
        .expect("append");
    ledger
        .append(reversal(
            "t1",
            "enroll-1-rev",
            UsageSource::ConfirmedEnrollment,
            at,
            "enroll-1",
        ))
        .expect("reversal");

    let total = ledger
        .flow_total(&tenant("t1"), &period_of(at), MeterId::Enrollments)
        .expect("total");
    assert_eq!(total, 0);
    // The original remains in the log: the ledger is append-only.
    assert_eq!(ledger.entries().len(), 2);
}

#[test]
fn an_entry_can_be_reversed_only_once() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 10, 0, 0);
    ledger
        .append(flow(
            "t1",
            "enroll-1",
            UsageSource::ConfirmedEnrollment,
            at,
            1,
        ))
        .expect("append");
    ledger
        .append(reversal(
            "t1",
            "rev-a",
            UsageSource::ConfirmedEnrollment,
            at,
            "enroll-1",
        ))
        .expect("first reversal");
    assert_eq!(
        ledger.append(reversal(
            "t1",
            "rev-b",
            UsageSource::ConfirmedEnrollment,
            at,
            "enroll-1"
        )),
        Err(LedgerError::AlreadyReversed)
    );
}

#[test]
fn reversal_must_target_an_existing_entry_of_the_same_tenant() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 10, 0, 0);
    ledger
        .append(flow(
            "t1",
            "enroll-1",
            UsageSource::ConfirmedEnrollment,
            at,
            1,
        ))
        .expect("append");

    assert_eq!(
        ledger.append(reversal(
            "t1",
            "rev-x",
            UsageSource::ConfirmedEnrollment,
            at,
            "missing"
        )),
        Err(LedgerError::ReversalTargetMissing)
    );
    // Another tenant cannot reverse t1's entry.
    assert_eq!(
        ledger.append(reversal(
            "t2",
            "rev-y",
            UsageSource::ConfirmedEnrollment,
            at,
            "enroll-1"
        )),
        Err(LedgerError::ReversalTargetMissing)
    );
}

#[test]
fn reversal_of_a_level_sample_or_with_a_different_source_is_not_permitted() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 0, 0, 0);
    ledger
        .append(sample("t1", "tmpl-day", UsageSource::TemplateSample, at, 4))
        .expect("sample");
    // A level source cannot carry a reversal: the source/body check rejects it first.
    assert_eq!(
        ledger.append(reversal(
            "t1",
            "rev-level",
            UsageSource::TemplateSample,
            at,
            "tmpl-day"
        )),
        Err(LedgerError::SourceMeterMismatch)
    );

    ledger
        .append(flow(
            "t1",
            "enroll-1",
            UsageSource::ConfirmedEnrollment,
            at,
            1,
        ))
        .expect("flow");
    assert_eq!(
        ledger.append(reversal(
            "t1",
            "rev-wrong-source",
            UsageSource::EligibleIdentificationCompleted,
            at,
            "enroll-1"
        )),
        Err(LedgerError::ReversalNotPermitted)
    );
}

// ---- Tenant isolation ----

#[test]
fn totals_are_scoped_to_the_tenant() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 10, 0, 0);
    ledger
        .append(flow("t1", "a", UsageSource::ConfirmedEnrollment, at, 5))
        .expect("t1");
    ledger
        .append(flow("t2", "b", UsageSource::ConfirmedEnrollment, at, 7))
        .expect("t2");

    let period = period_of(at);
    assert_eq!(
        ledger
            .flow_total(&tenant("t1"), &period, MeterId::Enrollments)
            .expect("t1"),
        5
    );
    assert_eq!(
        ledger
            .flow_total(&tenant("t2"), &period, MeterId::Enrollments)
            .expect("t2"),
        7
    );
    assert_eq!(
        ledger
            .flow_total(&tenant("t3"), &period, MeterId::Enrollments)
            .expect("t3"),
        0
    );
}

// ---- Month boundaries ----

#[test]
fn usage_is_booked_to_the_utc_month_of_its_occurrence() {
    let mut ledger = LedgerModel::new();
    ledger
        .append(flow(
            "t1",
            "jan-last",
            UsageSource::ConfirmedEnrollment,
            utc(2026, 1, 31, 23, 59, 59),
            1,
        ))
        .expect("jan");
    ledger
        .append(flow(
            "t1",
            "feb-first",
            UsageSource::ConfirmedEnrollment,
            utc(2026, 2, 1, 0, 0, 0),
            1,
        ))
        .expect("feb");

    let t = tenant("t1");
    let jan = period_of(utc(2026, 1, 15, 0, 0, 0));
    let feb = period_of(utc(2026, 2, 15, 0, 0, 0));
    assert_eq!(
        ledger
            .flow_total(&t, &jan, MeterId::Enrollments)
            .expect("jan"),
        1
    );
    assert_eq!(
        ledger
            .flow_total(&t, &feb, MeterId::Enrollments)
            .expect("feb"),
        1
    );
}

#[test]
fn a_correction_is_booked_against_the_original_period_not_the_later_one() {
    let mut ledger = LedgerModel::new();
    ledger
        .append(flow(
            "t1",
            "jan-enroll",
            UsageSource::ConfirmedEnrollment,
            utc(2026, 1, 30, 12, 0, 0),
            3,
        ))
        .expect("jan");
    // Reversal recorded in February, but it reduces January's total and never drives February negative.
    ledger
        .append(reversal(
            "t1",
            "jan-rev",
            UsageSource::ConfirmedEnrollment,
            utc(2026, 2, 2, 9, 0, 0),
            "jan-enroll",
        ))
        .expect("reversal");

    let t = tenant("t1");
    let jan = period_of(utc(2026, 1, 1, 0, 0, 0));
    let feb = period_of(utc(2026, 2, 1, 0, 0, 0));
    assert_eq!(
        ledger
            .flow_total(&t, &jan, MeterId::Enrollments)
            .expect("jan"),
        0
    );
    assert_eq!(
        ledger
            .flow_total(&t, &feb, MeterId::Enrollments)
            .expect("feb"),
        0
    );
}

// ---- Allowances: Free and paid, measured through the existing overage model ----

fn candidate() -> PlanCatalog {
    PlanCatalog::from_json_str(CANDIDATE).expect("candidate catalog")
}

fn measured_for(
    ledger: &LedgerModel,
    tenant_id: &TenantId,
    period: &AllowancePeriod,
) -> PerMeter<u64> {
    PerMeter {
        enrollments: ledger
            .flow_total(tenant_id, period, MeterId::Enrollments)
            .expect("enrollments"),
        retained_photo_storage_gb: 0,
        active_templates: 0,
        identification_requests: ledger
            .flow_total(tenant_id, period, MeterId::IdentificationRequests)
            .expect("identification"),
    }
}

#[test]
fn free_plan_blocks_usage_above_its_included_allowance() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 10, 0, 0);
    let t = tenant("free-1");
    // Free included 1,000 enrollments. Record 1,001 flows.
    for i in 0..1_001 {
        ledger
            .append(flow(
                "free-1",
                &format!("e-{i}"),
                UsageSource::ConfirmedEnrollment,
                at,
                1,
            ))
            .expect("append");
    }
    let period = period_of(at);
    let measured = measured_for(&ledger, &t, &period);

    let catalog = candidate();
    let free = &catalog.plan(PlanId::Free).expect("free").versions[0];
    let assessed = assess_usage(free, &measured).expect("assessment");
    let enrollments = assessed
        .meters
        .iter()
        .find(|m| m.meter == MeterId::Enrollments)
        .expect("meter");
    assert_eq!(enrollments.outcome, MeterOutcome::NotBillableAboveAllowance);
    assert_eq!(assessed.overage_total.micros(), 0);
}

#[test]
fn gold_plan_bills_only_the_excess_over_its_included_allowance() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 10, 0, 0);
    let t = tenant("gold-1");
    // Gold included 10,000. Record 10,003 confirmed enrollments in one batch of flows.
    for i in 0..10_003u64 {
        ledger
            .append(flow(
                "gold-1",
                &format!("g-{i}"),
                UsageSource::ConfirmedEnrollment,
                at,
                1,
            ))
            .expect("append");
    }
    let measured = measured_for(&ledger, &t, &period_of(at));

    let catalog = candidate();
    let gold = &catalog.plan(PlanId::Gold).expect("gold").versions[0];
    let assessed = assess_usage(gold, &measured).expect("assessment");
    // 3 excess * $0.15 = $0.45
    assert_eq!(assessed.overage_total.micros(), 450_000);
}

#[test]
fn annual_subscribers_are_accounted_monthly_regardless_of_base_interval() {
    let mut ledger = LedgerModel::new();
    let at = utc(2026, 11, 3, 10, 0, 0);
    let t = tenant("gold-annual");
    for i in 0..10_001u64 {
        ledger
            .append(flow(
                "gold-annual",
                &format!("a-{i}"),
                UsageSource::ConfirmedEnrollment,
                at,
                1,
            ))
            .expect("append");
    }
    let measured = measured_for(&ledger, &t, &period_of(at));

    let catalog = candidate();
    let gold = &catalog.plan(PlanId::Gold).expect("gold").versions[0];
    let assessed = assess_usage(gold, &measured).expect("assessment");
    // Usage is identical for monthly and annual subscribers: the annual base is not part of usage.
    assert_eq!(assessed.overage_total.micros(), 150_000);
    assert_eq!(
        gold.base_price(BillingInterval::Annual).micros(),
        990_000_000
    );
}

// ---- Level meters: template-month sampling and GiB-month storage ----

#[test]
fn template_month_is_the_average_of_daily_samples_rounded_up() {
    let mut ledger = LedgerModel::new();
    let t = tenant("tmpl");
    // Days 1..=10 with 3 templates, days 11..=30 with 5 templates.
    // Sum = 10*3 + 20*5 = 130 over 30 days, so 4.33 template-months, rounded up to 5.
    for d in 1..=30u32 {
        let units = if d <= 10 { 3 } else { 5 };
        ledger
            .append(sample(
                "tmpl",
                &format!("t-{d}"),
                UsageSource::TemplateSample,
                utc(2026, 11, d, 0, 0, 0),
                units,
            ))
            .expect("sample");
    }
    let quantity = ledger
        .level_billable_quantity(
            &t,
            &period_of(utc(2026, 11, 1, 0, 0, 0)),
            MeterId::ActiveTemplates,
            day(2026, 11, 1),
            day(2026, 11, 30),
        )
        .expect("quantity");
    assert_eq!(quantity, 5);
}

#[test]
fn a_later_sample_for_the_same_day_supersedes_the_earlier_one() {
    let mut ledger = LedgerModel::new();
    let t = tenant("tmpl");
    ledger
        .append(sample(
            "tmpl",
            "s-early",
            UsageSource::TemplateSample,
            utc(2026, 11, 1, 0, 0, 0),
            100,
        ))
        .expect("early");
    ledger
        .append(sample(
            "tmpl",
            "s-late",
            UsageSource::TemplateSample,
            utc(2026, 11, 1, 6, 0, 0),
            2,
        ))
        .expect("late");
    let quantity = ledger
        .level_billable_quantity(
            &t,
            &period_of(utc(2026, 11, 1, 0, 0, 0)),
            MeterId::ActiveTemplates,
            day(2026, 11, 1),
            day(2026, 11, 1),
        )
        .expect("quantity");
    assert_eq!(quantity, 2);
}

#[test]
fn a_missing_daily_sample_fails_instead_of_undercounting() {
    let mut ledger = LedgerModel::new();
    let t = tenant("tmpl");
    ledger
        .append(sample(
            "tmpl",
            "d1",
            UsageSource::TemplateSample,
            utc(2026, 11, 1, 0, 0, 0),
            4,
        ))
        .expect("d1");
    // Day 2 has no sample.
    ledger
        .append(sample(
            "tmpl",
            "d3",
            UsageSource::TemplateSample,
            utc(2026, 11, 3, 0, 0, 0),
            4,
        ))
        .expect("d3");
    let result = ledger.level_billable_quantity(
        &t,
        &period_of(utc(2026, 11, 1, 0, 0, 0)),
        MeterId::ActiveTemplates,
        day(2026, 11, 1),
        day(2026, 11, 3),
    );
    assert_eq!(
        result,
        Err(LedgerError::MissingLevelSample {
            day: day(2026, 11, 2)
        })
    );
}

#[test]
fn partial_month_uses_only_the_billable_window_as_the_divisor() {
    let mut ledger = LedgerModel::new();
    let t = tenant("tmpl");
    // Feb 2026 (28 days). Tenant is billable only from Feb 1 to Feb 10, with 7 templates each day.
    for d in 1..=10u32 {
        ledger
            .append(sample(
                "tmpl",
                &format!("p-{d}"),
                UsageSource::TemplateSample,
                utc(2026, 2, d, 0, 0, 0),
                7,
            ))
            .expect("sample");
    }
    let quantity = ledger
        .level_billable_quantity(
            &t,
            &period_of(utc(2026, 2, 1, 0, 0, 0)),
            MeterId::ActiveTemplates,
            day(2026, 2, 1),
            day(2026, 2, 10),
        )
        .expect("quantity");
    assert_eq!(quantity, 7);
}

#[test]
fn storage_gib_month_averages_daily_bytes_and_rounds_up_to_whole_gib() {
    let mut ledger = LedgerModel::new();
    let t = tenant("store");
    // Days 1..=15 hold 2 GiB, days 16..=30 hold 0 (photos deleted mid-month). Average 1 GiB exactly.
    for d in 1..=30u32 {
        let bytes = if d <= 15 { 2 * BYTES_PER_GIB as u64 } else { 0 };
        ledger
            .append(sample(
                "store",
                &format!("st-{d}"),
                UsageSource::PhotoStorageSample,
                utc(2026, 11, d, 0, 0, 0),
                bytes,
            ))
            .expect("sample");
    }
    let quantity = ledger
        .level_billable_quantity(
            &t,
            &period_of(utc(2026, 11, 1, 0, 0, 0)),
            MeterId::RetainedPhotoStorageGb,
            day(2026, 11, 1),
            day(2026, 11, 30),
        )
        .expect("quantity");
    assert_eq!(quantity, 1);
}

#[test]
fn one_extra_byte_on_average_rounds_storage_up_to_the_next_gib() {
    let mut ledger = LedgerModel::new();
    let t = tenant("store");
    for d in 1..=30u32 {
        let bytes = if d <= 15 { 2 * BYTES_PER_GIB as u64 } else { 0 };
        let bytes = if d == 30 { bytes + 1 } else { bytes };
        ledger
            .append(sample(
                "store",
                &format!("st-{d}"),
                UsageSource::PhotoStorageSample,
                utc(2026, 11, d, 0, 0, 0),
                bytes,
            ))
            .expect("sample");
    }
    let quantity = ledger
        .level_billable_quantity(
            &t,
            &period_of(utc(2026, 11, 1, 0, 0, 0)),
            MeterId::RetainedPhotoStorageGb,
            day(2026, 11, 1),
            day(2026, 11, 30),
        )
        .expect("quantity");
    assert_eq!(quantity, 2);
}

#[test]
fn level_window_must_lie_within_one_period_and_be_ordered() {
    let ledger = LedgerModel::new();
    let t = tenant("tmpl");
    let period = period_of(utc(2026, 11, 1, 0, 0, 0));
    assert_eq!(
        ledger.level_billable_quantity(
            &t,
            &period,
            MeterId::ActiveTemplates,
            day(2026, 11, 5),
            day(2026, 11, 1)
        ),
        Err(LedgerError::InvalidWindow)
    );
    assert_eq!(
        ledger.level_billable_quantity(
            &t,
            &period,
            MeterId::ActiveTemplates,
            day(2026, 10, 31),
            day(2026, 11, 2)
        ),
        Err(LedgerError::InvalidWindow)
    );
    assert_eq!(
        ledger.level_billable_quantity(
            &t,
            &period,
            MeterId::ActiveTemplates,
            day(2026, 11, 1),
            day(2026, 12, 1)
        ),
        Err(LedgerError::InvalidWindow)
    );
}

#[test]
fn flow_meters_cannot_be_queried_as_levels() {
    let ledger = LedgerModel::new();
    let result = ledger.level_billable_quantity(
        &tenant("t"),
        &period_of(utc(2026, 11, 1, 0, 0, 0)),
        MeterId::Enrollments,
        day(2026, 11, 1),
        day(2026, 11, 1),
    );
    assert_eq!(result, Err(LedgerError::SourceMeterMismatch));
}

// ---- Reconciliation ----

#[test]
fn reconciliation_reports_drift_and_never_adjusts_silently() {
    assert_eq!(reconcile(10, 10), Reconciliation::Match);
    assert_eq!(
        reconcile(10, 9),
        Reconciliation::Drift {
            ledger: 10,
            authoritative: 9
        }
    );
}

#[test]
fn error_codes_are_stable() {
    assert_eq!(LedgerError::KeyConflict.code(), "LEDGER_KEY_CONFLICT");
    assert_eq!(
        LedgerError::AlreadyReversed.code(),
        "LEDGER_ALREADY_REVERSED"
    );
    assert_eq!(
        LedgerError::MissingLevelSample {
            day: day(2026, 11, 2)
        }
        .code(),
        "LEDGER_LEVEL_SAMPLE_MISSING"
    );
}
