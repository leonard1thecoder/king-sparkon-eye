//! Usage ledger model (issue #148, storage-independent core).
//!
//! The ledger is an **append-only** record of billable usage per tenant, keyed by a unique event
//! key so that replays and retries cannot double-count. Corrections are explicit reversals that
//! reference the original entry. Aggregation is per tenant and per UTC allowance period.
//!
//! [`LedgerModel`] is the reference semantics for the ledger. It is **not durable** and must not
//! be used for production enforcement or invoicing. The PostgreSQL adapter, transactional outbox
//! and Stripe Billing Meter reporting are blocked on #113, #28, #138 and #143 and are not
//! implemented here. A future adapter must reproduce these rules, enforced by unique constraints
//! inside a transaction.
//!
//! Source trust: the ledger does not verify that an enrollment or identification was
//! authoritatively confirmed. Callers must only build an [`AppendRequest`] from an authoritative
//! confirmation, and that contract is not yet agreed (#138, backend #66).

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, Utc};

use super::{AllowancePeriod, MeasurementKind, MeterId};

/// Bytes per gibibyte. Retained-photo storage is billed per whole GiB-month (candidate; unapproved).
pub const BYTES_PER_GIB: u128 = 1024 * 1024 * 1024;

const MAX_TENANT_LEN: usize = 64;
const MAX_KEY_LEN: usize = 128;

/// Opaque tenant identifier. Tenant identity from API credentials is not yet defined (#138).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TenantId(String);

impl TenantId {
    pub fn new(raw: &str) -> Result<Self, LedgerError> {
        let valid = !raw.is_empty()
            && raw.len() <= MAX_TENANT_LEN
            && raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if valid {
            Ok(Self(raw.to_owned()))
        } else {
            Err(LedgerError::InvalidTenant)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Unique idempotency key for one billable occurrence. Scoped by tenant.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EventKey(String);

impl EventKey {
    pub fn new(raw: &str) -> Result<Self, LedgerError> {
        let valid = !raw.is_empty()
            && raw.len() <= MAX_KEY_LEN
            && raw
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
        if valid {
            Ok(Self(raw.to_owned()))
        } else {
            Err(LedgerError::InvalidEventKey)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The authoritative occurrence that produced an entry. Each source maps to exactly one meter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsageSource {
    /// A successful, confirmed, billable enrollment. Requires the authoritative confirmation contract.
    ConfirmedEnrollment,
    /// An eligible identification request that completed through inference.
    EligibleIdentificationCompleted,
    /// A daily sample of active biometric templates for the tenant.
    TemplateSample,
    /// A daily sample of retained original-photo bytes for the tenant.
    PhotoStorageSample,
}

impl UsageSource {
    pub fn meter(self) -> MeterId {
        match self {
            UsageSource::ConfirmedEnrollment => MeterId::Enrollments,
            UsageSource::EligibleIdentificationCompleted => MeterId::IdentificationRequests,
            UsageSource::TemplateSample => MeterId::ActiveTemplates,
            UsageSource::PhotoStorageSample => MeterId::RetainedPhotoStorageGb,
        }
    }
}

/// Payload of a request to append to the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendBody {
    /// Counted flow for a flow meter. Quantity must be positive.
    Flow { quantity: u64 },
    /// Observed level for a level meter, in template count or retained bytes, at `occurred_at`.
    Level { units: u64 },
    /// Compensating reversal of an earlier flow entry. It is attributed to the original's period.
    Reversal { reverses: EventKey },
}

/// A request to append one ledger entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendRequest {
    pub tenant: TenantId,
    pub key: EventKey,
    pub source: UsageSource,
    pub occurred_at: DateTime<Utc>,
    pub body: AppendBody,
}

/// A stored, immutable ledger entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgerEntry {
    pub seq: u64,
    pub tenant: TenantId,
    pub key: EventKey,
    pub source: UsageSource,
    pub occurred_at: DateTime<Utc>,
    /// Allowance period the entry is booked against. Reversals inherit their original's period.
    pub period: AllowancePeriod,
    pub body: AppendBody,
}

/// Result of an append. Duplicates return the original sequence and never change state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOutcome {
    Appended { seq: u64 },
    Duplicate { seq: u64 },
}

/// Typed ledger errors. Messages never include biometric data, identifiers beyond the field name, or payloads.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LedgerError {
    #[error("tenant identifier is invalid")]
    InvalidTenant,
    #[error("event key is invalid")]
    InvalidEventKey,
    #[error("event key was already used for a different entry")]
    KeyConflict,
    #[error("source does not match the meter kind")]
    SourceMeterMismatch,
    #[error("flow quantity must be positive")]
    InvalidQuantity,
    #[error("reversal target does not exist for this tenant")]
    ReversalTargetMissing,
    #[error("reversal is not permitted for this entry")]
    ReversalNotPermitted,
    #[error("entry was already reversed")]
    AlreadyReversed,
    #[error("no level sample exists for a billable day")]
    MissingLevelSample { day: NaiveDate },
    #[error("billable window is empty or outside the allowance period")]
    InvalidWindow,
    #[error("ledger arithmetic overflowed")]
    ArithmeticOverflow,
}

impl LedgerError {
    pub fn code(&self) -> &'static str {
        match self {
            LedgerError::InvalidTenant => "LEDGER_TENANT_INVALID",
            LedgerError::InvalidEventKey => "LEDGER_EVENT_KEY_INVALID",
            LedgerError::KeyConflict => "LEDGER_KEY_CONFLICT",
            LedgerError::SourceMeterMismatch => "LEDGER_SOURCE_METER_MISMATCH",
            LedgerError::InvalidQuantity => "LEDGER_QUANTITY_INVALID",
            LedgerError::ReversalTargetMissing => "LEDGER_REVERSAL_TARGET_MISSING",
            LedgerError::ReversalNotPermitted => "LEDGER_REVERSAL_NOT_PERMITTED",
            LedgerError::AlreadyReversed => "LEDGER_ALREADY_REVERSED",
            LedgerError::MissingLevelSample { .. } => "LEDGER_LEVEL_SAMPLE_MISSING",
            LedgerError::InvalidWindow => "LEDGER_WINDOW_INVALID",
            LedgerError::ArithmeticOverflow => "LEDGER_ARITHMETIC_OVERFLOW",
        }
    }
}

/// Reference semantics for the usage ledger. Not durable: see the module documentation.
#[derive(Debug, Default)]
pub struct LedgerModel {
    entries: Vec<LedgerEntry>,
    /// (tenant, key) -> index into `entries`.
    index: HashMap<(TenantId, EventKey), usize>,
    /// (tenant, original key) of flow entries that have been reversed.
    reversed: HashSet<(TenantId, EventKey)>,
}

impl LedgerModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> &[LedgerEntry] {
        &self.entries
    }

    /// Appends an entry or returns the original for an identical replay.
    pub fn append(&mut self, request: AppendRequest) -> Result<AppendOutcome, LedgerError> {
        let storage_key = (request.tenant.clone(), request.key.clone());

        if let Some(&existing_index) = self.index.get(&storage_key) {
            let existing = &self.entries[existing_index];
            let same = existing.source == request.source
                && existing.occurred_at == request.occurred_at
                && existing.body == request.body;
            return if same {
                Ok(AppendOutcome::Duplicate { seq: existing.seq })
            } else {
                Err(LedgerError::KeyConflict)
            };
        }

        validate_source_and_body(request.source, &request.body)?;

        let (period, body) = match &request.body {
            AppendBody::Flow { quantity } => {
                if *quantity == 0 {
                    return Err(LedgerError::InvalidQuantity);
                }
                (
                    AllowancePeriod::containing(request.occurred_at),
                    request.body.clone(),
                )
            }
            AppendBody::Level { .. } => (
                AllowancePeriod::containing(request.occurred_at),
                request.body.clone(),
            ),
            AppendBody::Reversal { reverses } => {
                let original_index = *self
                    .index
                    .get(&(request.tenant.clone(), reverses.clone()))
                    .ok_or(LedgerError::ReversalTargetMissing)?;
                let original = &self.entries[original_index];

                let AppendBody::Flow { .. } = original.body else {
                    return Err(LedgerError::ReversalNotPermitted);
                };
                if original.source != request.source {
                    return Err(LedgerError::ReversalNotPermitted);
                }
                if self
                    .reversed
                    .contains(&(request.tenant.clone(), reverses.clone()))
                {
                    return Err(LedgerError::AlreadyReversed);
                }
                // Booked against the original's period, so no period can go negative.
                (
                    original.period,
                    AppendBody::Reversal {
                        reverses: reverses.clone(),
                    },
                )
            }
        };

        let seq = self.entries.len() as u64 + 1;
        if let AppendBody::Reversal { reverses } = &body {
            self.reversed
                .insert((request.tenant.clone(), reverses.clone()));
        }
        self.entries.push(LedgerEntry {
            seq,
            tenant: request.tenant.clone(),
            key: request.key.clone(),
            source: request.source,
            occurred_at: request.occurred_at,
            period,
            body,
        });
        self.index.insert(storage_key, self.entries.len() - 1);
        Ok(AppendOutcome::Appended { seq })
    }

    /// Net counted flow for one tenant, meter and period: increments minus reversals.
    pub fn flow_total(
        &self,
        tenant: &TenantId,
        period: &AllowancePeriod,
        meter: MeterId,
    ) -> Result<u64, LedgerError> {
        let mut total: i128 = 0;
        for entry in self.entries_for(tenant, period, meter) {
            match &entry.body {
                AppendBody::Flow { quantity } => total += i128::from(*quantity),
                AppendBody::Reversal { reverses } => {
                    let original = self.original_quantity(tenant, reverses)?;
                    total -= i128::from(original);
                }
                AppendBody::Level { .. } => {}
            }
        }
        u64::try_from(total).map_err(|_| LedgerError::ArithmeticOverflow)
    }

    /// Billable quantity for a level meter over an inclusive window of days within one period.
    ///
    /// Each day uses its latest sample. Every day in the window must have one, or the call fails
    /// (no silent undercount). The result is the average over the window, rounded up:
    /// template-months for templates, and GiB-months for photo storage.
    pub fn level_billable_quantity(
        &self,
        tenant: &TenantId,
        period: &AllowancePeriod,
        meter: MeterId,
        first_day: NaiveDate,
        last_day: NaiveDate,
    ) -> Result<u64, LedgerError> {
        if first_day > last_day
            || first_day < period.start_date()
            || last_day >= period.end_exclusive_date()
        {
            return Err(LedgerError::InvalidWindow);
        }
        if meter.measurement() != MeasurementKind::Level {
            return Err(LedgerError::SourceMeterMismatch);
        }

        let mut latest: HashMap<NaiveDate, (u64, u64)> = HashMap::new();
        for entry in self.entries_for(tenant, period, meter) {
            if let AppendBody::Level { units } = entry.body {
                let day = entry.occurred_at.date_naive();
                // Later sequence supersedes an earlier sample for the same day.
                latest.insert(day, (entry.seq, units));
            }
        }

        let window_days = days_inclusive(first_day, last_day)?;
        let mut sum: u128 = 0;
        let mut day = first_day;
        loop {
            let (_, units) = latest
                .get(&day)
                .copied()
                .ok_or(LedgerError::MissingLevelSample { day })?;
            sum = sum
                .checked_add(u128::from(units))
                .ok_or(LedgerError::ArithmeticOverflow)?;
            if day == last_day {
                break;
            }
            day = day.succ_opt().ok_or(LedgerError::InvalidWindow)?;
        }

        let divisor = match meter {
            MeterId::ActiveTemplates => u128::from(window_days),
            MeterId::RetainedPhotoStorageGb => u128::from(window_days)
                .checked_mul(BYTES_PER_GIB)
                .ok_or(LedgerError::ArithmeticOverflow)?,
            _ => return Err(LedgerError::SourceMeterMismatch),
        };

        let quantity = ceil_div(sum, divisor);
        u64::try_from(quantity).map_err(|_| LedgerError::ArithmeticOverflow)
    }

    fn entries_for<'a>(
        &'a self,
        tenant: &'a TenantId,
        period: &'a AllowancePeriod,
        meter: MeterId,
    ) -> impl Iterator<Item = &'a LedgerEntry> + 'a {
        self.entries.iter().filter(move |entry| {
            &entry.tenant == tenant && &entry.period == period && entry.source.meter() == meter
        })
    }

    fn original_quantity(&self, tenant: &TenantId, key: &EventKey) -> Result<u64, LedgerError> {
        let index = *self
            .index
            .get(&(tenant.clone(), key.clone()))
            .ok_or(LedgerError::ReversalTargetMissing)?;
        match self.entries[index].body {
            AppendBody::Flow { quantity } => Ok(quantity),
            _ => Err(LedgerError::ReversalNotPermitted),
        }
    }
}

fn validate_source_and_body(source: UsageSource, body: &AppendBody) -> Result<(), LedgerError> {
    let meter = source.meter();
    match body {
        AppendBody::Flow { .. } if meter.measurement() == MeasurementKind::Flow => Ok(()),
        AppendBody::Level { .. } if meter.measurement() == MeasurementKind::Level => Ok(()),
        AppendBody::Reversal { .. } if meter.measurement() == MeasurementKind::Flow => Ok(()),
        _ => Err(LedgerError::SourceMeterMismatch),
    }
}

fn days_inclusive(first: NaiveDate, last: NaiveDate) -> Result<u32, LedgerError> {
    let span = (last - first).num_days();
    let days = u32::try_from(span + 1).map_err(|_| LedgerError::InvalidWindow)?;
    Ok(days)
}

fn ceil_div(numerator: u128, denominator: u128) -> u128 {
    numerator.div_ceil(denominator)
}

/// Outcome of comparing ledger totals with an authoritative inventory. Reconciliation must
/// surface drift for review, never silently adjust a total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconciliation {
    Match,
    Drift { ledger: u64, authoritative: u64 },
}

pub fn reconcile(ledger: u64, authoritative: u64) -> Reconciliation {
    if ledger == authoritative {
        Reconciliation::Match
    } else {
        Reconciliation::Drift {
            ledger,
            authoritative,
        }
    }
}
