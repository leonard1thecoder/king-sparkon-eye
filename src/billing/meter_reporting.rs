//! Stripe Billing Meter event transport for issue #143.
//!
//! This is deliberately a transport adapter, not a billing decision engine. Callers must supply
//! a reconciled monthly aggregate and an authoritative Stripe customer mapping. Meter event names
//! are explicit configuration; no candidate name, price, tenant ID, or production default is
//! inferred here. Reporting is disabled unless explicitly enabled by deployment configuration.

use chrono::{Datelike, NaiveDate};
use reqwest::{Client, StatusCode};
use sha2::{Digest, Sha256};
use std::time::Duration;

use super::MeterId;

const STRIPE_METER_EVENT_URL: &str = "https://api.stripe.com/v1/billing/meter_events";
const MAX_EVENT_NAME_LEN: usize = 100;
const MAX_IDENTIFIER_LEN: usize = 100;

#[derive(Clone)]
pub struct StripeMeterConfig {
    /// Must remain false until #138/#143 approval gates are satisfied.
    pub enabled: bool,
    /// Restricted Stripe secret key. Never log or expose this value.
    pub secret_key: String,
    /// Stripe meter payload key configured for customer mapping (normally stripe_customer_id).
    pub customer_payload_key: String,
    /// Exact event names created in the Stripe account, keyed by meter.
    pub event_names: MeterEventNames,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeterEventNames {
    pub enrollments: String,
    pub retained_photo_storage_gb: String,
    pub active_templates: String,
    pub identification_requests: String,
}

impl MeterEventNames {
    pub fn for_meter(&self, meter: MeterId) -> &str {
        match meter {
            MeterId::Enrollments => &self.enrollments,
            MeterId::RetainedPhotoStorageGb => &self.retained_photo_storage_gb,
            MeterId::ActiveTemplates => &self.active_templates,
            MeterId::IdentificationRequests => &self.identification_requests,
        }
    }

    fn validate(&self) -> Result<(), StripeMeterError> {
        for name in [
            &self.enrollments,
            &self.retained_photo_storage_gb,
            &self.active_templates,
            &self.identification_requests,
        ] {
            if name.is_empty()
                || name.len() > MAX_EVENT_NAME_LEN
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' )
            {
                return Err(StripeMeterError::InvalidConfiguration);
            }
        }
        Ok(())
    }
}

/// One monthly aggregate, already calculated from the tenant-scoped ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonthlyMeterAggregate {
    /// Opaque, authoritative Stripe customer ID resolved from trusted billing-account state.
    pub stripe_customer_id: String,
    pub meter: MeterId,
    /// First day of the UTC usage month.
    pub period_start: NaiveDate,
    /// Aggregate quantity in the configured meter's unit.
    pub quantity: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum StripeMeterError {
    #[error("Stripe meter reporting is disabled")]
    Disabled,
    #[error("Stripe meter reporting configuration is invalid")]
    InvalidConfiguration,
    #[error("Stripe meter event request failed")]
    Transport,
    #[error("Stripe rejected the meter event")]
    Rejected,
    #[error("Stripe meter event returned an unexpected response")]
    UnexpectedResponse,
}

#[derive(Clone)]
pub struct StripeMeterReporter {
    client: Client,
    config: StripeMeterConfig,
}

impl StripeMeterReporter {
    pub fn new(config: StripeMeterConfig) -> Result<Self, StripeMeterError> {
        if !config.enabled {
            return Err(StripeMeterError::Disabled);
        }
        if !config.secret_key.starts_with("sk_test_")
            && !config.secret_key.starts_with("sk_live_")
        {
            return Err(StripeMeterError::InvalidConfiguration);
        }
        if config.customer_payload_key.is_empty()
            || config.customer_payload_key.len() > 64
            || !config.customer_payload_key.bytes().all(|b| {
                b.is_ascii_alphanumeric() || b == b'_'
            })
        {
            return Err(StripeMeterError::InvalidConfiguration);
        }
        config.event_names.validate()?;
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| StripeMeterError::Transport)?;
        Ok(Self { client, config })
    }

    /// Submit exactly one monthly aggregate. The stable Stripe identifier makes retries of the
    /// same tenant/meter/period aggregate deduplicable by Stripe; local ledger idempotency remains
    /// authoritative. No raw ledger rows or source payloads are sent.
    pub async fn report_monthly(
        &self,
        tenant_reference: &str,
        aggregate: &MonthlyMeterAggregate,
    ) -> Result<(), StripeMeterError> {
        if aggregate.quantity == 0
            || tenant_reference.is_empty()
            || tenant_reference.len() > 128
            || !tenant_reference.bytes().all(|b| {
                b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.' | b':')
            })
            || !aggregate.stripe_customer_id.starts_with("cus_")
            || aggregate.stripe_customer_id.len() > 64
            || !aggregate.stripe_customer_id.bytes().all(|b| {
                b.is_ascii_alphanumeric() || b == b'_'
            })
            || aggregate.period_start.day() != 1
        {
            return Err(StripeMeterError::InvalidConfiguration);
        }

        let event_name = self.config.event_names.for_meter(aggregate.meter);
        let identifier = report_identifier(
            tenant_reference,
            aggregate.meter,
            aggregate.period_start,
        );
        let timestamp = aggregate
            .period_start
            .and_hms_opt(0, 0, 0)
            .ok_or(StripeMeterError::InvalidConfiguration)?
            .and_utc()
            .timestamp();

        // Stripe accepts string values in the payload map. The customer mapping key must match
        // the meter's configured customer mapping in Stripe; this adapter never guesses it.
        let customer_key = format!("payload[{}]", self.config.customer_payload_key);
        let form = vec![
            ("event_name", event_name.to_owned()),
            ("identifier", identifier),
            ("timestamp", timestamp.to_string()),
            (customer_key.as_str(), aggregate.stripe_customer_id.clone()),
            ("payload[value]", aggregate.quantity.to_string()),
        ];

        let response = self
            .client
            .post(STRIPE_METER_EVENT_URL)
            .bearer_auth(&self.config.secret_key)
            .form(&form)
            .send()
            .await
            .map_err(|_| StripeMeterError::Transport)?;

        match response.status() {
            StatusCode::OK | StatusCode::CREATED => Ok(()),
            status if status.is_client_error() || status.is_server_error() => {
                Err(StripeMeterError::Rejected)
            }
            _ => Err(StripeMeterError::UnexpectedResponse),
        }
    }
}

/// Stable, opaque identifier scoped to one tenant, meter and UTC month. The raw tenant reference
/// is never sent to Stripe; only this digest-derived identifier is transmitted.
pub fn report_identifier(
    tenant_reference: &str,
    meter: MeterId,
    period_start: NaiveDate,
) -> String {
    let mut digest = Sha256::new();
    digest.update(tenant_reference.as_bytes());
    digest.update([0]);
    digest.update(meter_key(meter).as_bytes());
    digest.update([0]);
    digest.update(period_start.format("%Y-%m-%d").to_string().as_bytes());
    let hash = digest.finalize();
    let hex = hash.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
    format!("eye_{hex}")
}

fn meter_key(meter: MeterId) -> &'static str {
    match meter {
        MeterId::Enrollments => "enrollments",
        MeterId::RetainedPhotoStorageGb => "retained_photo_storage_gb",
        MeterId::ActiveTemplates => "active_templates",
        MeterId::IdentificationRequests => "identification_requests",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_identifier_is_stable_and_scoped_to_meter_and_period() {
        let tenant = "tenant_opaque_123";
        let month = NaiveDate::from_ymd_opt(2026, 11, 1).unwrap();
        let first = report_identifier(tenant, MeterId::Enrollments, month);
        assert_eq!(first, report_identifier(tenant, MeterId::Enrollments, month));
        assert_ne!(first, report_identifier(tenant, MeterId::IdentificationRequests, month));
        assert_ne!(
            first,
            report_identifier(tenant, MeterId::Enrollments, NaiveDate::from_ymd_opt(2026, 12, 1).unwrap())
        );
        assert!(first.starts_with("eye_"));
        assert_eq!(first.len(), 68);
    }

    #[test]
    fn event_names_must_be_explicit_and_safe() {
        let names = MeterEventNames {
            enrollments: "eye_enrollment".into(),
            retained_photo_storage_gb: "eye_photo_gb".into(),
            active_templates: "eye_templates".into(),
            identification_requests: "eye_identification".into(),
        };
        assert!(names.validate().is_ok());
        let mut invalid = names;
        invalid.enrollments = "approved?no".into();
        assert_eq!(invalid.validate(), Err(StripeMeterError::InvalidConfiguration));
    }

    #[test]
    fn meter_names_are_not_derived_from_candidate_prices() {
        let names = MeterEventNames {
            enrollments: "configured_enrollments".into(),
            retained_photo_storage_gb: "configured_photos".into(),
            active_templates: "configured_templates".into(),
            identification_requests: "configured_identifications".into(),
        };
        assert_eq!(names.for_meter(MeterId::ActiveTemplates), "configured_templates");
    }
}
