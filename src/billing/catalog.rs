//! Validated billing plan catalog (issue #145).
//!
//! The catalog is a JSON document that holds candidate prices, allowances and overage rates.
//! Parsing happens in two steps: a raw serde layer shapes the JSON and rejects unknown,
//! missing, duplicate or negative fields, then [`PlanCatalog`] validates the business rules
//! (required plans, version order, contradictory pricing) and converts amounts to exact
//! micro-units. Amounts are decimal strings such as `"0.015"`, so no float is ever parsed.
//!
//! The catalog is not loaded by the service runtime yet. It is a configuration model and test
//! fixture until quota enforcement (#146) and Stripe catalog work (#139/#147) consume it.

use std::collections::HashSet;

use chrono::{DateTime, NaiveDate, Utc};
use serde::Deserialize;

use super::{
    AllowancePeriod, BasePrice, BillingError, Currency, MICROS_PER_UNIT, MeterId, Money, Overage,
    PerMeter, Plan, PlanId, PlanVersion,
};

/// The only catalog schema version this build understands.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// Approval state recorded in the catalog. Only the candidate state can be loaded today:
/// approval evidence and its gate belong to the Stripe catalog work (#139/#147).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    CandidateUnapproved,
}

/// A fully validated catalog. Every plan in [`PlanId::ALL`] is present exactly once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanCatalog {
    pub schema_version: u32,
    pub currency: Currency,
    pub approval_status: ApprovalStatus,
    pub plans: Vec<Plan>,
}

impl PlanCatalog {
    /// Parses and validates a catalog JSON document.
    ///
    /// Errors never echo the submitted amounts or other field values.
    pub fn from_json_str(input: &str) -> Result<Self, BillingError> {
        let raw: RawCatalog =
            serde_json::from_str(input).map_err(|err| BillingError::Malformed {
                line: err.line(),
                column: err.column(),
            })?;
        Self::try_from(raw)
    }

    pub fn plan(&self, id: PlanId) -> Option<&Plan> {
        self.plans.iter().find(|plan| plan.id == id)
    }
}

impl Plan {
    /// The pricing version in force at `instant`: the latest version whose effective date
    /// (00:00:00Z) is at or before the instant. `None` before the first version.
    pub fn version_in_force_at(&self, instant: DateTime<Utc>) -> Option<&PlanVersion> {
        self.versions
            .iter()
            .rev()
            .find(|version| effective_instant(version.effective_from) <= instant)
    }

    /// The version that governs a whole allowance period. It is the version in force at the
    /// period's start, so a change made during a month applies from the next period.
    pub fn version_for_period(&self, period: &AllowancePeriod) -> Option<&PlanVersion> {
        self.version_in_force_at(period.start_instant())
    }
}

fn effective_instant(date: NaiveDate) -> DateTime<Utc> {
    date.and_hms_opt(0, 0, 0)
        .expect("midnight exists for every date")
        .and_utc()
}

// ---- Raw wire shape. Strict: unknown fields are rejected, numbers are unsigned. ----

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCatalog {
    schema_version: u32,
    currency: Currency,
    approval_status: ApprovalStatus,
    plans: Vec<RawPlan>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlan {
    id: PlanId,
    versions: Vec<RawVersion>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawVersion {
    version: u32,
    effective_from: String,
    base: RawBase,
    included: RawPerMeter<u64>,
    overage: RawOverage,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBase {
    monthly: String,
    annual: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOverage {
    mode: RawOverageMode,
    rates: Option<RawPerMeter<String>>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RawOverageMode {
    Block,
    Bill,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPerMeter<T> {
    enrollments: T,
    retained_photo_storage_gb: T,
    active_templates: T,
    identification_requests: T,
}

impl<T> RawPerMeter<T> {
    fn into_per_meter<U>(
        self,
        mut convert: impl FnMut(MeterId, T) -> Result<U, BillingError>,
    ) -> Result<PerMeter<U>, BillingError> {
        Ok(PerMeter {
            enrollments: convert(MeterId::Enrollments, self.enrollments)?,
            retained_photo_storage_gb: convert(
                MeterId::RetainedPhotoStorageGb,
                self.retained_photo_storage_gb,
            )?,
            active_templates: convert(MeterId::ActiveTemplates, self.active_templates)?,
            identification_requests: convert(
                MeterId::IdentificationRequests,
                self.identification_requests,
            )?,
        })
    }
}

// ---- Validation into domain types. ----

impl TryFrom<RawCatalog> for PlanCatalog {
    type Error = BillingError;

    fn try_from(raw: RawCatalog) -> Result<Self, BillingError> {
        if raw.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(BillingError::UnsupportedSchemaVersion(raw.schema_version));
        }

        let mut seen = HashSet::new();
        let mut plans = Vec::with_capacity(raw.plans.len());
        for raw_plan in raw.plans {
            if !seen.insert(raw_plan.id) {
                return Err(BillingError::DuplicatePlan(raw_plan.id));
            }
            plans.push(validate_plan(raw_plan)?);
        }

        for required in PlanId::ALL {
            if !seen.contains(&required) {
                return Err(BillingError::MissingPlan(required));
            }
        }

        Ok(PlanCatalog {
            schema_version: raw.schema_version,
            currency: raw.currency,
            approval_status: raw.approval_status,
            plans,
        })
    }
}

fn validate_plan(raw: RawPlan) -> Result<Plan, BillingError> {
    let id = raw.id;
    if raw.versions.is_empty() {
        return Err(BillingError::NoVersions(id));
    }

    let mut versions: Vec<PlanVersion> = Vec::with_capacity(raw.versions.len());
    for (index, raw_version) in raw.versions.into_iter().enumerate() {
        let expected = u32::try_from(index + 1).map_err(|_| BillingError::VersionSequence(id))?;
        if raw_version.version != expected {
            return Err(BillingError::VersionSequence(id));
        }

        let effective_from = NaiveDate::parse_from_str(&raw_version.effective_from, "%Y-%m-%d")
            .map_err(|_| BillingError::InvalidEffectiveDate(id))?;
        if let Some(previous) = versions.last()
            && effective_from <= previous.effective_from
        {
            return Err(BillingError::EffectiveDateOrder(id));
        }

        let version = validate_version(id, raw_version.version, effective_from, raw_version)?;
        versions.push(version);
    }

    Ok(Plan { id, versions })
}

fn validate_version(
    plan: PlanId,
    version: u32,
    effective_from: NaiveDate,
    raw: RawVersion,
) -> Result<PlanVersion, BillingError> {
    let monthly = parse_money(plan, "base.monthly", &raw.base.monthly)?;
    let annual = parse_money(plan, "base.annual", &raw.base.annual)?;
    check_base_prices(plan, monthly, annual)?;

    let included = raw
        .included
        .into_per_meter(|_, value| Ok::<u64, BillingError>(value))?;

    let is_free = monthly.is_zero() && annual.is_zero();
    let overage = match (raw.overage.mode, raw.overage.rates) {
        (RawOverageMode::Block, Some(_)) => {
            return Err(BillingError::Contradictory {
                plan,
                reason: "blocked overage must not define billable rates",
            });
        }
        (RawOverageMode::Block, None) => {
            if !is_free {
                return Err(BillingError::Contradictory {
                    plan,
                    reason: "a paid plan must bill overage, not block it",
                });
            }
            Overage::Block
        }
        (RawOverageMode::Bill, None) => {
            return Err(BillingError::Contradictory {
                plan,
                reason: "billed overage requires a rate for every meter",
            });
        }
        (RawOverageMode::Bill, Some(rates)) => {
            if is_free {
                return Err(BillingError::Contradictory {
                    plan,
                    reason: "a zero-priced plan must block overage, not bill it",
                });
            }
            let rates = rates.into_per_meter(|meter, raw_rate| {
                let field = rate_field_name(meter);
                let rate = parse_money(plan, field, &raw_rate)?;
                if rate.is_zero() {
                    return Err(BillingError::InvalidAmount { plan, field });
                }
                Ok(rate)
            })?;
            Overage::Bill(rates)
        }
    };

    Ok(PlanVersion {
        version,
        effective_from,
        base: BasePrice { monthly, annual },
        included,
        overage,
    })
}

fn check_base_prices(plan: PlanId, monthly: Money, annual: Money) -> Result<(), BillingError> {
    if monthly.is_zero() != annual.is_zero() {
        return Err(BillingError::Contradictory {
            plan,
            reason: "monthly and annual base prices must both be zero or both be positive",
        });
    }
    if let Some(twelve_months) = monthly.checked_mul_quantity(12)
        && annual > twelve_months
    {
        return Err(BillingError::Contradictory {
            plan,
            reason: "annual base price exceeds twelve monthly payments",
        });
    }
    Ok(())
}

fn rate_field_name(meter: MeterId) -> &'static str {
    match meter {
        MeterId::Enrollments => "overage.rates.enrollments",
        MeterId::RetainedPhotoStorageGb => "overage.rates.retained_photo_storage_gb",
        MeterId::ActiveTemplates => "overage.rates.active_templates",
        MeterId::IdentificationRequests => "overage.rates.identification_requests",
    }
}

/// Parses a non-negative decimal amount into exact micro-units.
///
/// Accepts `0`, `99`, `99.00` and `0.015`. Rejects signs, exponents, separators, blanks,
/// empty fractions, more than six fractional digits, and values that overflow `u64` micros.
pub fn parse_money(plan: PlanId, field: &'static str, raw: &str) -> Result<Money, BillingError> {
    let invalid = BillingError::InvalidAmount { plan, field };

    let (whole, fraction) = match raw.split_once('.') {
        Some((whole, fraction)) => {
            if fraction.is_empty()
                || fraction.len() > 6
                || !fraction.bytes().all(|b| b.is_ascii_digit())
            {
                return Err(invalid);
            }
            (whole, fraction)
        }
        None => (raw, ""),
    };

    if whole.is_empty() || !whole.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid);
    }

    let whole_units: u64 = whole.parse().map_err(|_| invalid.clone())?;
    let fraction_micros: u64 = format!("{fraction:0<6}")
        .parse()
        .map_err(|_| invalid.clone())?;

    let micros = whole_units
        .checked_mul(MICROS_PER_UNIT)
        .and_then(|value| value.checked_add(fraction_micros))
        .ok_or_else(|| invalid.clone())?;

    Ok(Money::from_micros(micros))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_exact_decimal_amounts() {
        let plan = PlanId::Gold;
        assert_eq!(parse_money(plan, "f", "0").expect("valid").micros(), 0);
        assert_eq!(
            parse_money(plan, "f", "99").expect("valid").micros(),
            99_000_000
        );
        assert_eq!(
            parse_money(plan, "f", "99.00").expect("valid").micros(),
            99_000_000
        );
        assert_eq!(
            parse_money(plan, "f", "0.015").expect("valid").micros(),
            15_000
        );
        assert_eq!(
            parse_money(plan, "f", "0.000001").expect("valid").micros(),
            1
        );
    }

    #[test]
    fn rejects_malformed_amounts() {
        let plan = PlanId::Gold;
        for bad in [
            "",
            "-1",
            "+1",
            "1e3",
            " 1",
            "1 ",
            "1.2.3",
            "1,000",
            "1.",
            ".5",
            "0.0000001",
            "abc",
            "18446744073709551615",
        ] {
            assert!(
                matches!(
                    parse_money(plan, "f", bad),
                    Err(BillingError::InvalidAmount { .. })
                ),
                "expected {bad:?} to be rejected"
            );
        }
    }
}
