//! Per-registered-account Stripe Customer mapping (#243).
//!
//! Ownership is the immutable Eye user ID from a verified Eye JWT subject. Tracker tenant
//! identity is deliberately not part of this store. Customer IDs are server-owned and scoped
//! by Stripe mode; callers must never pass a customer ID from a request body.

use sqlx::{PgPool, Row};
use thiserror::Error;

use crate::billing::stripe::{StripeClient, StripeMode};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BillingAccountStoreError {
    #[error("billing account identifier is invalid")]
    InvalidOwner,
    #[error("Stripe Customer mapping conflicts with an existing account mapping")]
    MappingConflict,
    #[error("billing account storage is unavailable")]
    Unavailable,
}

impl From<sqlx::Error> for BillingAccountStoreError {
    fn from(_: sqlx::Error) -> Self {
        Self::Unavailable
    }
}

#[derive(Clone)]
pub struct BillingAccountStore {
    pool: PgPool,
}

impl BillingAccountStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Resolve the mapped Customer for this Eye account and exact Stripe mode.
    pub async fn customer_for(
        &self,
        eye_user_id: &str,
        mode: StripeMode,
    ) -> Result<Option<String>, BillingAccountStoreError> {
        validate_eye_user_id(eye_user_id)?;
        let row: Option<String> = sqlx::query_scalar(
            "SELECT stripe_customer_id FROM eye_billing_accounts WHERE eye_user_id = $1 AND stripe_mode = $2",
        )
        .bind(eye_user_id)
        .bind(mode.to_string())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    /// Persist a Customer returned by Stripe. Concurrent callers converge on the same
    /// durable mapping. A different Customer for an already-mapped owner is a hard conflict.
    pub async fn insert_customer_mapping(
        &self,
        eye_user_id: &str,
        mode: StripeMode,
        stripe_customer_id: &str,
    ) -> Result<String, BillingAccountStoreError> {
        validate_eye_user_id(eye_user_id)?;
        if !valid_customer_id(stripe_customer_id) {
            return Err(BillingAccountStoreError::MappingConflict);
        }

        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "INSERT INTO eye_billing_accounts (eye_user_id, stripe_mode, stripe_customer_id) VALUES ($1, $2, $3) ON CONFLICT (eye_user_id, stripe_mode) DO NOTHING",
        )
        .bind(eye_user_id)
        .bind(mode.to_string())
        .bind(stripe_customer_id)
        .execute(&mut *tx)
        .await?;

        let row = sqlx::query(
            "SELECT stripe_customer_id FROM eye_billing_accounts WHERE eye_user_id = $1 AND stripe_mode = $2 FOR UPDATE",
        )
        .bind(eye_user_id)
        .bind(mode.to_string())
        .fetch_one(&mut *tx)
        .await?;
        let persisted: String = row.try_get("stripe_customer_id")?;
        if persisted != stripe_customer_id {
            return Err(BillingAccountStoreError::MappingConflict);
        }
        tx.commit().await?;
        Ok(persisted)
    }
}

pub fn validate_eye_user_id(value: &str) -> Result<(), BillingAccountStoreError> {
    let suffix = value.strip_prefix("usr_").unwrap_or_default();
    if suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(BillingAccountStoreError::InvalidOwner)
    }
}

fn valid_customer_id(value: &str) -> bool {
    value.len() > 4
        && value.len() <= 255
        && value.starts_with("cus_")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Return the existing mapping or provision and persist one Customer for this Eye account.
/// Stripe's deterministic idempotency key makes concurrent calls converge on one Customer;
/// the database uniqueness constraints remain the durable ownership boundary.
pub async fn ensure_customer_mapping(
    store: &BillingAccountStore,
    stripe: &StripeClient,
    eye_user_id: &str,
) -> Result<String, BillingAccountProvisionError> {
    validate_eye_user_id(eye_user_id).map_err(|_| BillingAccountProvisionError::InvalidOwner)?;
    if let Some(customer_id) = store
        .customer_for(eye_user_id, stripe.mode())
        .await
        .map_err(|_| BillingAccountProvisionError::Unavailable)?
    {
        return Ok(customer_id);
    }
    let customer_id = stripe
        .create_customer(eye_user_id)
        .await
        .map_err(|_| BillingAccountProvisionError::StripeUnavailable)?;
    store
        .insert_customer_mapping(eye_user_id, stripe.mode(), &customer_id)
        .await
        .map_err(|error| match error {
            BillingAccountStoreError::MappingConflict => {
                BillingAccountProvisionError::MappingConflict
            },
            BillingAccountStoreError::InvalidOwner => BillingAccountProvisionError::InvalidOwner,
            BillingAccountStoreError::Unavailable => BillingAccountProvisionError::Unavailable,
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum BillingAccountProvisionError {
    #[error("billing account identifier is invalid")]
    InvalidOwner,
    #[error("Stripe Customer provisioning is temporarily unavailable")]
    StripeUnavailable,
    #[error("billing account mapping conflicts with an existing mapping")]
    MappingConflict,
    #[error("billing account storage is unavailable")]
    Unavailable,
}

/// Usage reporting receives the Eye account ID as its opaque tenant reference. The lookup is
/// strictly mode-scoped and never falls back to another mode or derives a Customer from email.
pub struct PgStripeCustomerResolver {
    store: BillingAccountStore,
    mode: StripeMode,
}

impl PgStripeCustomerResolver {
    pub fn new(store: BillingAccountStore, mode: StripeMode) -> Self {
        Self { store, mode }
    }
}

#[async_trait::async_trait]
impl crate::billing::reporting_worker::StripeCustomerResolver for PgStripeCustomerResolver {
    async fn resolve_customer(
        &self,
        account_reference: &str,
    ) -> Result<Option<String>, crate::billing::reporting_worker::CustomerResolveError> {
        self.store
            .customer_for(account_reference, self.mode)
            .await
            .map_err(|_| crate::billing::reporting_worker::CustomerResolveError::Unavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_eye_registered_account_subjects() {
        assert!(validate_eye_user_id(&format!("usr_{}", "a".repeat(32))).is_ok());
        assert!(validate_eye_user_id("tenant_123").is_err());
        assert!(validate_eye_user_id("usr_not-a-uuid").is_err());
        assert!(validate_eye_user_id("usr_").is_err());
    }

    #[test]
    fn stripe_customer_ids_are_opaque_and_strictly_validated() {
        assert!(valid_customer_id("cus_123abc"));
        assert!(!valid_customer_id("cus_"));
        assert!(!valid_customer_id("acct_123"));
        assert!(!valid_customer_id("cus_bad/id"));
    }
}
