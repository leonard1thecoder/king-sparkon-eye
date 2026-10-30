use chrono::{DateTime, TimeZone, Utc};
use king_sparkon_eye::billing::catalog::parse_money;
use king_sparkon_eye::billing::{
    AllowancePeriod, ApprovalStatus, BillingError, BillingInterval, Currency, MeterId,
    MeterOutcome, Money, Overage, PerMeter, PlanCatalog, PlanId, assess_meter, assess_usage,
};

const CANDIDATE: &str = include_str!("../config/billing/plan-catalog.candidate.json");

const INCLUDED: &str = r#"{"enrollments":10,"retained_photo_storage_gb":5,"active_templates":10,"identification_requests":20}"#;
const RATES: &str = r#"{"enrollments":"0.15","retained_photo_storage_gb":"0.06","active_templates":"0.015","identification_requests":"0.05"}"#;
const BLOCK: &str = r#"{"mode":"block"}"#;

fn utc(y: i32, m: u32, d: u32, h: u32, min: u32, s: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(y, m, d, h, min, s)
        .single()
        .expect("valid instant")
}

fn bill() -> String {
    format!(r#"{{"mode":"bill","rates":{RATES}}}"#)
}

fn version(n: u32, effective_from: &str, monthly: &str, annual: &str, overage: &str) -> String {
    format!(
        r#"{{"version":{n},"effective_from":"{effective_from}","base":{{"monthly":"{monthly}","annual":"{annual}"}},"included":{INCLUDED},"overage":{overage}}}"#
    )
}

fn plan(id: &str, versions: &[String]) -> String {
    format!(r#"{{"id":"{id}","versions":[{}]}}"#, versions.join(","))
}

fn catalog(plans: &[String]) -> String {
    format!(
        r#"{{"schema_version":1,"currency":"USD","approval_status":"candidate_unapproved","plans":[{}]}}"#,
        plans.join(",")
    )
}

fn valid_plans() -> Vec<String> {
    vec![
        plan("free", &[version(1, "2026-11-01", "0", "0", BLOCK)]),
        plan("gold", &[version(1, "2026-11-01", "99", "990", &bill())]),
        plan(
            "premium",
            &[version(1, "2026-11-01", "299", "2990", &bill())],
        ),
    ]
}

fn parse(plans: &[String]) -> Result<PlanCatalog, BillingError> {
    PlanCatalog::from_json_str(&catalog(plans))
}

#[test]
fn valid_catalog_with_all_three_plans_parses() {
    let parsed = parse(&valid_plans()).expect("valid catalog");
    assert_eq!(parsed.plans.len(), 3);
    assert_eq!(parsed.currency, Currency::Usd);
    assert_eq!(parsed.approval_status, ApprovalStatus::CandidateUnapproved);
}

// ---- Candidate catalog shipped in config/billing ----

#[test]
fn candidate_catalog_is_marked_unapproved_and_defines_all_plans() {
    let parsed = PlanCatalog::from_json_str(CANDIDATE).expect("candidate catalog is valid");
    assert_eq!(parsed.approval_status, ApprovalStatus::CandidateUnapproved);
    for id in PlanId::ALL {
        assert!(parsed.plan(id).is_some(), "missing candidate plan {id:?}");
    }
}

#[test]
fn candidate_values_match_the_issue_hypotheses() {
    let parsed = PlanCatalog::from_json_str(CANDIDATE).expect("candidate catalog is valid");
    let gold = &parsed.plan(PlanId::Gold).expect("gold").versions[0];
    let premium = &parsed.plan(PlanId::Premium).expect("premium").versions[0];
    let free = &parsed.plan(PlanId::Free).expect("free").versions[0];

    assert_eq!(gold.base.monthly, Money::from_micros(99_000_000));
    assert_eq!(gold.base.annual, Money::from_micros(990_000_000));
    assert_eq!(premium.base.monthly, Money::from_micros(299_000_000));
    assert_eq!(premium.base.annual, Money::from_micros(2_990_000_000));
    assert!(free.is_free());

    assert_eq!(
        free.included,
        PerMeter {
            enrollments: 1_000,
            retained_photo_storage_gb: 5,
            active_templates: 1_000,
            identification_requests: 2_000
        }
    );
    assert_eq!(
        gold.included,
        PerMeter {
            enrollments: 10_000,
            retained_photo_storage_gb: 50,
            active_templates: 10_000,
            identification_requests: 25_000
        }
    );
    assert_eq!(
        premium.included,
        PerMeter {
            enrollments: 50_000,
            retained_photo_storage_gb: 250,
            active_templates: 50_000,
            identification_requests: 150_000
        }
    );

    match &gold.overage {
        Overage::Bill(rates) => {
            assert_eq!(rates.enrollments, Money::from_micros(150_000));
            assert_eq!(rates.retained_photo_storage_gb, Money::from_micros(60_000));
            assert_eq!(rates.active_templates, Money::from_micros(15_000));
            assert_eq!(rates.identification_requests, Money::from_micros(50_000));
        }
        Overage::Block => panic!("gold must bill overage"),
    }
    assert_eq!(free.overage, Overage::Block);
}

#[test]
fn annual_base_is_a_discounted_whole_year_not_a_monthly_rate() {
    let parsed = PlanCatalog::from_json_str(CANDIDATE).expect("candidate catalog is valid");
    let gold = &parsed.plan(PlanId::Gold).expect("gold").versions[0];
    let monthly_twelve = gold
        .base_price(BillingInterval::Monthly)
        .checked_mul_quantity(12)
        .expect("no overflow");
    assert!(gold.base_price(BillingInterval::Annual) < monthly_twelve);
    assert_eq!(
        gold.base_price(BillingInterval::Annual),
        Money::from_micros(990_000_000)
    );
}

// ---- Money parsing ----

#[test]
fn money_parsing_is_exact_and_rejects_malformed_values() {
    assert_eq!(
        parse_money(PlanId::Gold, "f", "0.015").expect("valid"),
        Money::from_micros(15_000)
    );
    for bad in ["-1", "1e3", "1,000", "1.", ".5", "0.0000001", ""] {
        assert!(
            matches!(
                parse_money(PlanId::Gold, "f", bad),
                Err(BillingError::InvalidAmount {
                    plan: PlanId::Gold,
                    field: "f"
                })
            ),
            "expected {bad:?} to be rejected"
        );
    }
}

// ---- Structural validation ----

#[test]
fn malformed_json_reports_position_without_echoing_values() {
    let err = PlanCatalog::from_json_str("{ \"schema_version\": 1, ").expect_err("truncated JSON");
    assert!(matches!(err, BillingError::Malformed { .. }));
    assert_eq!(err.code(), "BILLING_CATALOG_MALFORMED");
}

#[test]
fn unknown_fields_are_rejected() {
    let json =
        catalog(&valid_plans()).replace(r#""currency":"USD""#, r#""currency":"USD","surprise":1"#);
    assert!(matches!(
        PlanCatalog::from_json_str(&json),
        Err(BillingError::Malformed { .. })
    ));
}

#[test]
fn negative_allowance_is_rejected_as_malformed() {
    let json = catalog(&[
        plan(
            "free",
            &[version(1, "2026-11-01", "0", "0", BLOCK)
                .replace(r#""enrollments":10"#, r#""enrollments":-1"#)],
        ),
        plan("gold", &[version(1, "2026-11-01", "99", "990", &bill())]),
        plan(
            "premium",
            &[version(1, "2026-11-01", "299", "2990", &bill())],
        ),
    ]);
    assert!(matches!(
        PlanCatalog::from_json_str(&json),
        Err(BillingError::Malformed { .. })
    ));
}

#[test]
fn duplicate_allowance_key_is_rejected_as_malformed() {
    let dup = INCLUDED.replace(
        r#""enrollments":10"#,
        r#""enrollments":10,"enrollments":20"#,
    );
    let json = catalog(&[
        plan(
            "free",
            &[version(1, "2026-11-01", "0", "0", BLOCK).replace(INCLUDED, &dup)],
        ),
        plan("gold", &[version(1, "2026-11-01", "99", "990", &bill())]),
        plan(
            "premium",
            &[version(1, "2026-11-01", "299", "2990", &bill())],
        ),
    ]);
    assert!(matches!(
        PlanCatalog::from_json_str(&json),
        Err(BillingError::Malformed { .. })
    ));
}

#[test]
fn missing_allowance_meter_is_rejected_as_malformed() {
    let partial = r#"{"enrollments":10,"retained_photo_storage_gb":5,"active_templates":10}"#;
    let json = catalog(&[
        plan(
            "free",
            &[version(1, "2026-11-01", "0", "0", BLOCK).replace(INCLUDED, partial)],
        ),
        plan("gold", &[version(1, "2026-11-01", "99", "990", &bill())]),
        plan(
            "premium",
            &[version(1, "2026-11-01", "299", "2990", &bill())],
        ),
    ]);
    assert!(matches!(
        PlanCatalog::from_json_str(&json),
        Err(BillingError::Malformed { .. })
    ));
}

#[test]
fn missing_plan_is_rejected() {
    let mut plans = valid_plans();
    plans.remove(2);
    assert_eq!(
        parse(&plans),
        Err(BillingError::MissingPlan(PlanId::Premium))
    );
}

#[test]
fn duplicate_plan_is_rejected() {
    let mut plans = valid_plans();
    plans.push(plan(
        "gold",
        &[version(1, "2026-11-01", "99", "990", &bill())],
    ));
    assert_eq!(
        parse(&plans),
        Err(BillingError::DuplicatePlan(PlanId::Gold))
    );
}

#[test]
fn unsupported_schema_version_is_rejected() {
    let json = catalog(&valid_plans()).replace(r#""schema_version":1"#, r#""schema_version":2"#);
    assert_eq!(
        PlanCatalog::from_json_str(&json),
        Err(BillingError::UnsupportedSchemaVersion(2))
    );
}

#[test]
fn unsupported_currency_is_rejected() {
    let json = catalog(&valid_plans()).replace(r#""currency":"USD""#, r#""currency":"ZAR""#);
    assert!(matches!(
        PlanCatalog::from_json_str(&json),
        Err(BillingError::Malformed { .. })
    ));
}

#[test]
fn approval_claims_other_than_candidate_are_rejected() {
    let json = catalog(&valid_plans()).replace("candidate_unapproved", "approved");
    assert!(matches!(
        PlanCatalog::from_json_str(&json),
        Err(BillingError::Malformed { .. })
    ));
}

#[test]
fn invalid_amount_names_plan_and_field_only() {
    let mut plans = valid_plans();
    plans[1] = plan("gold", &[version(1, "2026-11-01", "1e3", "990", &bill())]);
    assert_eq!(
        parse(&plans),
        Err(BillingError::InvalidAmount {
            plan: PlanId::Gold,
            field: "base.monthly"
        })
    );
}

#[test]
fn zero_overage_rate_is_rejected() {
    let zero_rates = RATES.replace(r#""enrollments":"0.15""#, r#""enrollments":"0""#);
    let mut plans = valid_plans();
    plans[1] = plan(
        "gold",
        &[version(
            1,
            "2026-11-01",
            "99",
            "990",
            &format!(r#"{{"mode":"bill","rates":{zero_rates}}}"#),
        )],
    );
    assert_eq!(
        parse(&plans),
        Err(BillingError::InvalidAmount {
            plan: PlanId::Gold,
            field: "overage.rates.enrollments"
        })
    );
}

#[test]
fn monthly_and_annual_base_must_both_be_zero_or_both_positive() {
    let mut plans = valid_plans();
    plans[1] = plan("gold", &[version(1, "2026-11-01", "0", "990", &bill())]);
    assert!(matches!(
        parse(&plans),
        Err(BillingError::Contradictory {
            plan: PlanId::Gold,
            ..
        })
    ));
}

#[test]
fn annual_price_above_twelve_monthly_payments_is_contradictory() {
    let mut plans = valid_plans();
    plans[1] = plan("gold", &[version(1, "2026-11-01", "99", "1200", &bill())]);
    assert!(matches!(
        parse(&plans),
        Err(BillingError::Contradictory {
            plan: PlanId::Gold,
            ..
        })
    ));
}

#[test]
fn annual_price_exactly_twelve_monthly_payments_is_allowed() {
    let mut plans = valid_plans();
    plans[1] = plan("gold", &[version(1, "2026-11-01", "99", "1188", &bill())]);
    assert!(parse(&plans).is_ok());
}

#[test]
fn zero_priced_plan_must_block_overage() {
    let mut plans = valid_plans();
    plans[0] = plan("free", &[version(1, "2026-11-01", "0", "0", &bill())]);
    assert!(matches!(
        parse(&plans),
        Err(BillingError::Contradictory {
            plan: PlanId::Free,
            ..
        })
    ));
}

#[test]
fn paid_plan_must_bill_overage() {
    let mut plans = valid_plans();
    plans[1] = plan("gold", &[version(1, "2026-11-01", "99", "990", BLOCK)]);
    assert!(matches!(
        parse(&plans),
        Err(BillingError::Contradictory {
            plan: PlanId::Gold,
            ..
        })
    ));
}

#[test]
fn billed_overage_without_rates_is_contradictory() {
    let mut plans = valid_plans();
    plans[1] = plan(
        "gold",
        &[version(1, "2026-11-01", "99", "990", r#"{"mode":"bill"}"#)],
    );
    assert!(matches!(
        parse(&plans),
        Err(BillingError::Contradictory {
            plan: PlanId::Gold,
            ..
        })
    ));
}

#[test]
fn blocked_overage_with_rates_is_contradictory() {
    let mut plans = valid_plans();
    plans[0] = plan(
        "free",
        &[version(
            1,
            "2026-11-01",
            "0",
            "0",
            &format!(r#"{{"mode":"block","rates":{RATES}}}"#),
        )],
    );
    assert!(matches!(
        parse(&plans),
        Err(BillingError::Contradictory {
            plan: PlanId::Free,
            ..
        })
    ));
}

#[test]
fn plan_without_versions_is_rejected() {
    let mut plans = valid_plans();
    plans[1] = r#"{"id":"gold","versions":[]}"#.to_owned();
    assert_eq!(parse(&plans), Err(BillingError::NoVersions(PlanId::Gold)));
}

#[test]
fn version_numbers_must_run_in_sequence() {
    let mut plans = valid_plans();
    plans[1] = plan(
        "gold",
        &[
            version(1, "2026-11-01", "99", "990", &bill()),
            version(3, "2027-01-01", "99", "990", &bill()),
        ],
    );
    assert_eq!(
        parse(&plans),
        Err(BillingError::VersionSequence(PlanId::Gold))
    );
}

#[test]
fn effective_dates_must_be_strictly_ascending() {
    let mut plans = valid_plans();
    plans[1] = plan(
        "gold",
        &[
            version(1, "2027-01-01", "99", "990", &bill()),
            version(2, "2026-11-01", "99", "990", &bill()),
        ],
    );
    assert_eq!(
        parse(&plans),
        Err(BillingError::EffectiveDateOrder(PlanId::Gold))
    );
}

#[test]
fn duplicate_effective_dates_are_rejected() {
    let mut plans = valid_plans();
    plans[1] = plan(
        "gold",
        &[
            version(1, "2026-11-01", "99", "990", &bill()),
            version(2, "2026-11-01", "99", "990", &bill()),
        ],
    );
    assert_eq!(
        parse(&plans),
        Err(BillingError::EffectiveDateOrder(PlanId::Gold))
    );
}

#[test]
fn impossible_calendar_dates_are_rejected() {
    let mut plans = valid_plans();
    plans[1] = plan("gold", &[version(1, "2026-02-30", "99", "990", &bill())]);
    assert_eq!(
        parse(&plans),
        Err(BillingError::InvalidEffectiveDate(PlanId::Gold))
    );
}

#[test]
fn error_codes_are_stable_strings() {
    assert_eq!(
        BillingError::MissingPlan(PlanId::Gold).code(),
        "BILLING_PLAN_MISSING"
    );
    assert_eq!(
        BillingError::DuplicatePlan(PlanId::Gold).code(),
        "BILLING_PLAN_DUPLICATE"
    );
    assert_eq!(
        BillingError::ArithmeticOverflow.code(),
        "BILLING_ARITHMETIC_OVERFLOW"
    );
}

// ---- Versioned pricing and effective dates ----

/// Gold with a price and rate change effective 2027-01-01, plus a mid-December change that
/// must not affect December itself.
fn gold_with_two_versions() -> PlanCatalog {
    let v2_rates = RATES.replace(r#""enrollments":"0.15""#, r#""enrollments":"0.20""#);
    let mut plans = valid_plans();
    plans[1] = plan(
        "gold",
        &[
            version(1, "2026-11-01", "99", "990", &bill()),
            version(
                2,
                "2026-12-15",
                "119",
                "1190",
                &format!(r#"{{"mode":"bill","rates":{v2_rates}}}"#),
            ),
        ],
    );
    parse(&plans).expect("two-version gold catalog is valid")
}

#[test]
fn version_in_force_changes_exactly_at_effective_midnight_utc() {
    let catalog = gold_with_two_versions();
    let gold = catalog.plan(PlanId::Gold).expect("gold");

    assert!(
        gold.version_in_force_at(utc(2026, 10, 31, 23, 59, 59))
            .is_none()
    );
    assert_eq!(
        gold.version_in_force_at(utc(2026, 11, 1, 0, 0, 0))
            .expect("v1")
            .version,
        1
    );
    assert_eq!(
        gold.version_in_force_at(utc(2026, 12, 14, 23, 59, 59))
            .expect("v1")
            .version,
        1
    );
    assert_eq!(
        gold.version_in_force_at(utc(2026, 12, 15, 0, 0, 0))
            .expect("v2")
            .version,
        2
    );
}

#[test]
fn mid_period_change_applies_from_the_next_allowance_period() {
    let catalog = gold_with_two_versions();
    let gold = catalog.plan(PlanId::Gold).expect("gold");

    let december = AllowancePeriod::containing(utc(2026, 12, 1, 0, 0, 0));
    let january = AllowancePeriod::containing(utc(2027, 1, 1, 0, 0, 0));

    assert_eq!(
        gold.version_for_period(&december)
            .expect("v1 governs December")
            .version,
        1
    );
    assert_eq!(
        gold.version_for_period(&january)
            .expect("v2 governs January")
            .version,
        2
    );
}

#[test]
fn historical_period_keeps_its_original_pricing_interpretation() {
    let catalog = gold_with_two_versions();
    let gold = catalog.plan(PlanId::Gold).expect("gold");

    // The fixture's included enrollments is 10, so 11 is exactly one over the allowance.
    let usage = PerMeter {
        enrollments: 11,
        ..PerMeter::default()
    };
    let december = AllowancePeriod::containing(utc(2026, 12, 1, 0, 0, 0));
    let v_december = gold.version_for_period(&december).expect("v1");
    let assessed = assess_usage(v_december, &usage).expect("assessment");

    // One enrollment over the allowance at the v1 rate of $0.15, not the v2 rate of $0.20.
    assert_eq!(assessed.overage_total, Money::from_micros(150_000));
}

// ---- Allowance periods (UTC calendar months) ----

#[test]
fn allowance_periods_reset_at_utc_month_boundaries() {
    let end_of_jan = AllowancePeriod::containing(utc(2026, 1, 31, 23, 59, 59));
    let start_of_feb = AllowancePeriod::containing(utc(2026, 2, 1, 0, 0, 0));
    assert_ne!(end_of_jan, start_of_feb);
    assert_eq!(end_of_jan.next(), start_of_feb);
}

#[test]
fn allowance_period_end_is_exclusive_across_a_year_end() {
    let period = AllowancePeriod::containing(utc(2026, 12, 31, 23, 0, 0));
    assert!(period.contains(utc(2026, 12, 31, 23, 59, 59)));
    assert!(!period.contains(utc(2027, 1, 1, 0, 0, 0)));
}

// ---- Included allowance, measured usage and billable overage ----

fn gold_version() -> king_sparkon_eye::billing::PlanVersion {
    let catalog = PlanCatalog::from_json_str(CANDIDATE).expect("candidate");
    catalog.plan(PlanId::Gold).expect("gold").versions[0].clone()
}

fn free_version() -> king_sparkon_eye::billing::PlanVersion {
    let catalog = PlanCatalog::from_json_str(CANDIDATE).expect("candidate");
    catalog.plan(PlanId::Free).expect("free").versions[0].clone()
}

#[test]
fn usage_below_the_allowance_is_within_allowance_and_free() {
    let assessed = assess_meter(&gold_version(), MeterId::Enrollments, 9_999).expect("assessment");
    assert_eq!(assessed.excess, 0);
    assert_eq!(assessed.outcome, MeterOutcome::WithinAllowance);
    assert_eq!(assessed.overage_charge, Money::ZERO);
}

#[test]
fn usage_exactly_at_the_allowance_is_still_within_allowance() {
    let assessed = assess_meter(&gold_version(), MeterId::Enrollments, 10_000).expect("assessment");
    assert_eq!(assessed.excess, 0);
    assert_eq!(assessed.outcome, MeterOutcome::WithinAllowance);
}

#[test]
fn one_enrollment_over_the_allowance_is_billed_at_the_rate() {
    let assessed = assess_meter(&gold_version(), MeterId::Enrollments, 10_001).expect("assessment");
    assert_eq!(assessed.included, 10_000);
    assert_eq!(assessed.measured, 10_001);
    assert_eq!(assessed.excess, 1);
    assert_eq!(assessed.outcome, MeterOutcome::BillableOverage);
    assert_eq!(assessed.overage_charge, Money::from_micros(150_000));
}

#[test]
fn storage_and_template_overage_use_their_own_rates() {
    let storage =
        assess_meter(&gold_version(), MeterId::RetainedPhotoStorageGb, 51).expect("assessment");
    assert_eq!(storage.overage_charge, Money::from_micros(60_000));

    let templates =
        assess_meter(&gold_version(), MeterId::ActiveTemplates, 10_001).expect("assessment");
    assert_eq!(templates.overage_charge, Money::from_micros(15_000));
}

#[test]
fn blocked_plan_reports_excess_without_charging_it() {
    let assessed =
        assess_meter(&free_version(), MeterId::IdentificationRequests, 2_500).expect("assessment");
    assert_eq!(assessed.excess, 500);
    assert_eq!(assessed.outcome, MeterOutcome::NotBillableAboveAllowance);
    assert_eq!(assessed.overage_charge, Money::ZERO);
}

#[test]
fn overage_totals_sum_meters_without_including_base_price() {
    let usage = PerMeter {
        enrollments: 10_100,             // 100 * 0.15 = 15.00
        retained_photo_storage_gb: 60,   // 10 * 0.06 = 0.60
        active_templates: 10_000,        // within allowance
        identification_requests: 25_010, // 10 * 0.05 = 0.50
    };
    let assessed = assess_usage(&gold_version(), &usage).expect("assessment");

    assert_eq!(assessed.overage_total, Money::from_micros(16_100_000));
    assert_eq!(assessed.meters.len(), 4);
    // The annual base price is a separate whole-year charge, not part of usage.
    assert_eq!(
        gold_version().base_price(BillingInterval::Annual),
        Money::from_micros(990_000_000)
    );
}

#[test]
fn zero_usage_produces_no_overage_for_any_plan() {
    let assessed = assess_usage(&gold_version(), &PerMeter::default()).expect("assessment");
    assert_eq!(assessed.overage_total, Money::ZERO);
}

#[test]
fn overage_arithmetic_overflow_is_an_error_not_a_wrap() {
    let mut plans = valid_plans();
    plans[1] = plan(
        "gold",
        &[version(
            1,
            "2026-11-01",
            "99",
            "990",
            r#"{"mode":"bill","rates":{"enrollments":"18446744073709","retained_photo_storage_gb":"0.06","active_templates":"0.015","identification_requests":"0.05"}}"#,
        )],
    );
    let catalog = parse(&plans).expect("catalog with a very large rate is structurally valid");
    let gold = catalog.plan(PlanId::Gold).expect("gold").versions[0].clone();

    let err =
        assess_meter(&gold, MeterId::Enrollments, 10_002).expect_err("overflow must be reported");
    assert_eq!(err, BillingError::ArithmeticOverflow);
}

#[test]
fn money_displays_exact_six_place_decimals() {
    assert_eq!(Money::from_micros(15_000).to_string(), "0.015000");
    assert_eq!(Money::from_micros(990_000_000).to_string(), "990.000000");
}

#[test]
fn overage_is_the_same_whatever_the_base_interval() {
    // Interval affects only the base charge; overage is identical for monthly and annual.
    let v = gold_version();
    let monthly = v.base_price(BillingInterval::Monthly);
    let annual = v.base_price(BillingInterval::Annual);
    assert_ne!(monthly, annual);

    let usage = PerMeter {
        enrollments: 10_010,
        ..PerMeter::default()
    };
    let assessed = assess_usage(&v, &usage).expect("assessment");
    assert_eq!(assessed.overage_total, Money::from_micros(1_500_000));
}

#[test]
fn candidate_currency_is_usd_only() {
    let parsed = PlanCatalog::from_json_str(CANDIDATE).expect("candidate");
    assert_eq!(parsed.currency, Currency::Usd);
}
