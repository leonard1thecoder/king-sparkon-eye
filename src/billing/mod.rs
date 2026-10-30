//! Rust-owned billing domain for the proposed Free, Gold and Premium plans (issue #145).
//!
//! Every price, allowance and rate here is a **candidate** value loaded from a validated
//! configuration document. Nothing in this module is an approved production price, calls
//! Stripe, or reads or writes usage. Billing never grants, bypasses or measures consent,
//! enrollment authorization, ticket eligibility, worker authorization or final admission;
//! those remain owned by the Spring Boot backend and the existing Eye authorization layers.
//!
//! See `docs/BILLING_PLAN_DOMAIN.md` for the semantics (periods, versioning, overage rules).

pub mod catalog;
pub mod ledger;
pub mod period;
pub mod stripe;

use std::fmt;

use serde::{Deserialize, Serialize};

pub use catalog::{ApprovalStatus, PlanCatalog};
pub use period::AllowancePeriod;

/// Micro-units per whole currency unit. `$0.015` is `15_000` micro-units.
pub const MICROS_PER_UNIT: u64 = 1_000_000;

/// Plan identifiers. The set is closed: a catalog must define every variant exactly once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanId {
    Free,
    Gold,
    Premium,
}

impl PlanId {
    pub const ALL: [PlanId; 3] = [PlanId::Free, PlanId::Gold, PlanId::Premium];
}

/// Recurring base-subscription interval. Usage is always accounted monthly in arrears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BillingInterval {
    Monthly,
    Annual,
}

/// Supported billing currencies. Only USD is in the candidate model; ZAR is not modelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Currency {
    #[serde(rename = "USD")]
    Usd,
}

/// Usage meters. The set is closed and each meter has one fixed measurement kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeterId {
    /// Successful, confirmed, billable enrollments. A monthly flow.
    Enrollments,
    /// Retained original-photo storage in whole gigabytes. A monthly level.
    RetainedPhotoStorageGb,
    /// Active biometric templates. A monthly level.
    ActiveTemplates,
    /// Eligible completed identification requests. A monthly flow.
    IdentificationRequests,
}

impl MeterId {
    pub const ALL: [MeterId; 4] = [
        MeterId::Enrollments,
        MeterId::RetainedPhotoStorageGb,
        MeterId::ActiveTemplates,
        MeterId::IdentificationRequests,
    ];

    /// How the meter's measured quantity is produced. Flows are counted, levels are sampled.
    pub fn measurement(self) -> MeasurementKind {
        match self {
            MeterId::Enrollments | MeterId::IdentificationRequests => MeasurementKind::Flow,
            MeterId::RetainedPhotoStorageGb | MeterId::ActiveTemplates => MeasurementKind::Level,
        }
    }

    /// Stable, human-readable unit label used in documentation and error messages.
    pub fn unit_label(self) -> &'static str {
        match self {
            MeterId::Enrollments => "confirmed enrollment",
            MeterId::RetainedPhotoStorageGb => "retained GB-month",
            MeterId::ActiveTemplates => "active template-month",
            MeterId::IdentificationRequests => "eligible completed identification request",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeasurementKind {
    /// Counted events within one allowance period. The allowance resets each period.
    Flow,
    /// A quantity observed within the period (for example stored bytes or active templates).
    /// The allowance is a cap on that level and does not accumulate across periods.
    Level,
}

/// Exact money amount in micro-units of the currency. Never a floating-point value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Money {
    micros: u64,
}

impl Money {
    pub const ZERO: Money = Money { micros: 0 };

    pub const fn from_micros(micros: u64) -> Self {
        Self { micros }
    }

    pub const fn micros(self) -> u64 {
        self.micros
    }

    pub const fn is_zero(self) -> bool {
        self.micros == 0
    }

    /// Multiplies by a non-negative quantity, failing on overflow instead of wrapping.
    pub fn checked_mul_quantity(self, quantity: u64) -> Option<Money> {
        self.micros.checked_mul(quantity).map(Money::from_micros)
    }

    pub fn checked_add(self, other: Money) -> Option<Money> {
        self.micros
            .checked_add(other.micros)
            .map(Money::from_micros)
    }
}

impl fmt::Display for Money {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}.{:06}",
            self.micros / MICROS_PER_UNIT,
            self.micros % MICROS_PER_UNIT
        )
    }
}

/// One value per usage meter. Fields are fixed, so a value set cannot have duplicates or gaps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PerMeter<T> {
    pub enrollments: T,
    pub retained_photo_storage_gb: T,
    pub active_templates: T,
    pub identification_requests: T,
}

impl<T> PerMeter<T> {
    pub fn get(&self, meter: MeterId) -> &T {
        match meter {
            MeterId::Enrollments => &self.enrollments,
            MeterId::RetainedPhotoStorageGb => &self.retained_photo_storage_gb,
            MeterId::ActiveTemplates => &self.active_templates,
            MeterId::IdentificationRequests => &self.identification_requests,
        }
    }
}

/// Recurring base price per interval. Annual is the whole-year base subscription, not a monthly rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasePrice {
    pub monthly: Money,
    pub annual: Money,
}

/// How usage beyond the included allowance is treated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overage {
    /// Usage above the allowance is not billable. Enforcement (issue #146) must stop or refuse it.
    Block,
    /// Usage above the allowance is billed per unit at the configured rate for each meter.
    Bill(PerMeter<Money>),
}

/// One immutable pricing and allowance version. A change creates a new version with a new
/// effective date, so historical periods keep their original interpretation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanVersion {
    pub version: u32,
    /// The version applies from 00:00:00 UTC on this date onward.
    pub effective_from: chrono::NaiveDate,
    pub base: BasePrice,
    /// Included allowances per meter. Zero is allowed.
    pub included: PerMeter<u64>,
    pub overage: Overage,
}

impl PlanVersion {
    /// A plan is free when its base price is zero for both intervals.
    pub fn is_free(&self) -> bool {
        self.base.monthly.is_zero() && self.base.annual.is_zero()
    }

    pub fn base_price(&self, interval: BillingInterval) -> Money {
        match interval {
            BillingInterval::Monthly => self.base.monthly,
            BillingInterval::Annual => self.base.annual,
        }
    }
}

/// A plan with its versions sorted by effective date (strictly ascending).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub id: PlanId,
    pub versions: Vec<PlanVersion>,
}

/// Outcome of comparing one meter's measured quantity against its allowance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeterOutcome {
    WithinAllowance,
    BillableOverage,
    /// Above the allowance on a plan that blocks overage. Not billable; enforcement is #146.
    NotBillableAboveAllowance,
}

/// Included allowance, measured usage, excess and charge for one meter. These are kept separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeterAssessment {
    pub meter: MeterId,
    pub included: u64,
    pub measured: u64,
    /// `measured - included`, floored at zero.
    pub excess: u64,
    pub outcome: MeterOutcome,
    /// Charge for the excess. Zero unless the outcome is `BillableOverage`.
    pub overage_charge: Money,
}

/// Usage-only assessment for one allowance period. Base subscription charges are reported
/// separately through [`PlanVersion::base_price`], because annual base pricing is per year.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageAssessment {
    pub meters: Vec<MeterAssessment>,
    pub overage_total: Money,
}

/// Compares one meter's measured quantity to the included allowance and prices any excess.
///
/// `excess = max(0, measured - included)`. Overflow in the charge is an error, never a wrap.
pub fn assess_meter(
    version: &PlanVersion,
    meter: MeterId,
    measured: u64,
) -> Result<MeterAssessment, BillingError> {
    let included = *version.included.get(meter);
    let excess = measured.saturating_sub(included);

    let (outcome, overage_charge) = match (&version.overage, excess) {
        (_, 0) => (MeterOutcome::WithinAllowance, Money::ZERO),
        (Overage::Block, _) => (MeterOutcome::NotBillableAboveAllowance, Money::ZERO),
        (Overage::Bill(rates), _) => {
            let rate = *rates.get(meter);
            let charge = rate
                .checked_mul_quantity(excess)
                .ok_or(BillingError::ArithmeticOverflow)?;
            (MeterOutcome::BillableOverage, charge)
        }
    };

    Ok(MeterAssessment {
        meter,
        included,
        measured,
        excess,
        outcome,
        overage_charge,
    })
}

/// Assesses every meter for one allowance period against a plan version.
///
/// Missing measurements must be supplied as zero by the caller (usage aggregation is #148).
pub fn assess_usage(
    version: &PlanVersion,
    measured: &PerMeter<u64>,
) -> Result<UsageAssessment, BillingError> {
    let meters = MeterId::ALL
        .iter()
        .map(|meter| assess_meter(version, *meter, *measured.get(*meter)))
        .collect::<Result<Vec<_>, _>>()?;

    let overage_total = meters.iter().try_fold(Money::ZERO, |acc, m| {
        acc.checked_add(m.overage_charge)
            .ok_or(BillingError::ArithmeticOverflow)
    })?;

    Ok(UsageAssessment {
        meters,
        overage_total,
    })
}

/// Typed, stable billing configuration and calculation errors.
///
/// Errors name the plan, meter or field, never a configured amount's raw input text and never
/// any biometric, attendee or ticket data.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BillingError {
    #[error("billing catalog is not valid for the expected schema (line {line}, column {column})")]
    Malformed { line: usize, column: usize },
    #[error("unsupported billing catalog schema version {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("billing catalog does not define plan {0:?}")]
    MissingPlan(PlanId),
    #[error("billing catalog defines plan {0:?} more than once")]
    DuplicatePlan(PlanId),
    #[error("plan {0:?} has no pricing versions")]
    NoVersions(PlanId),
    #[error("plan {0:?} versions must be numbered 1, 2, 3 in order")]
    VersionSequence(PlanId),
    #[error("plan {0:?} effective dates must be strictly ascending")]
    EffectiveDateOrder(PlanId),
    #[error("plan {0:?} has an effective date that is not a valid YYYY-MM-DD date")]
    InvalidEffectiveDate(PlanId),
    #[error("plan {plan:?} has an invalid amount in field `{field}`")]
    InvalidAmount { plan: PlanId, field: &'static str },
    #[error("plan {plan:?} contradicts itself: {reason}")]
    Contradictory { plan: PlanId, reason: &'static str },
    #[error("no pricing version of plan {plan:?} is in force for the requested time")]
    NoEffectiveVersion { plan: PlanId },
    #[error("billing arithmetic overflowed")]
    ArithmeticOverflow,
}

impl BillingError {
    /// Stable machine-readable code for logs, tests and future API mapping.
    pub fn code(&self) -> &'static str {
        match self {
            BillingError::Malformed { .. } => "BILLING_CATALOG_MALFORMED",
            BillingError::UnsupportedSchemaVersion(_) => "BILLING_CATALOG_SCHEMA_UNSUPPORTED",
            BillingError::MissingPlan(_) => "BILLING_PLAN_MISSING",
            BillingError::DuplicatePlan(_) => "BILLING_PLAN_DUPLICATE",
            BillingError::NoVersions(_) => "BILLING_PLAN_NO_VERSIONS",
            BillingError::VersionSequence(_) => "BILLING_PLAN_VERSION_SEQUENCE",
            BillingError::EffectiveDateOrder(_) => "BILLING_PLAN_EFFECTIVE_DATE_ORDER",
            BillingError::InvalidEffectiveDate(_) => "BILLING_PLAN_EFFECTIVE_DATE_INVALID",
            BillingError::InvalidAmount { .. } => "BILLING_AMOUNT_INVALID",
            BillingError::Contradictory { .. } => "BILLING_PLAN_CONTRADICTORY",
            BillingError::NoEffectiveVersion { .. } => "BILLING_VERSION_NOT_EFFECTIVE",
            BillingError::ArithmeticOverflow => "BILLING_ARITHMETIC_OVERFLOW",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_displays_six_decimal_places() {
        assert_eq!(Money::from_micros(15_000).to_string(), "0.015000");
        assert_eq!(
            Money::from_micros(99 * MICROS_PER_UNIT).to_string(),
            "99.000000"
        );
    }

    #[test]
    fn every_meter_has_exactly_one_measurement_kind() {
        assert_eq!(MeterId::Enrollments.measurement(), MeasurementKind::Flow);
        assert_eq!(
            MeterId::IdentificationRequests.measurement(),
            MeasurementKind::Flow
        );
        assert_eq!(
            MeterId::RetainedPhotoStorageGb.measurement(),
            MeasurementKind::Level
        );
        assert_eq!(
            MeterId::ActiveTemplates.measurement(),
            MeasurementKind::Level
        );
    }

    #[test]
    fn checked_multiplication_reports_overflow_instead_of_wrapping() {
        let rate = Money::from_micros(u64::MAX);
        assert_eq!(rate.checked_mul_quantity(2), None);
    }
}
