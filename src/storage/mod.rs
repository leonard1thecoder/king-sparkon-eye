//! Private image storage and enrollment metadata boundary (issue #27).
//!
//! Stores the original photo of a passing, consented enrollment and records its metadata.
//! The AWS S3 adapter is tracked in #31. This module defines the interface that adapter
//! must implement.
//!
//! Rules enforced here:
//! - Only a `AssessmentPassed` result can create a durable record.
//! - The consent reference must be a validated identifier. Checking it against a real
//!   consent record is the job of issue #15, not this module.
//! - Object keys are random and opaque. They are never derived from the photo or the user.
//! - Rust reports persistence as `Pending`. It never reports `Confirmed`. Only Spring Boot
//!   confirms a record was persisted.
//! - A storage failure never produces a record. A failed record insert removes its object.
//! - Retries with the same idempotency key return the same record, or a conflict if the content differs.

pub mod s3;
pub use s3::*;

use std::collections::HashMap;
use std::sync::Mutex;

use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::contract::{PersistenceStatus, SuitabilityStatus, validate_identifier};
use crate::error::ValidationError;

/// Prefix for every object this service creates. Orphan cleanup only touches keys with this prefix.
pub const OBJECT_KEY_PREFIX: &str = "enrollments/";

/// Typed failures from an object store. Messages are fixed and carry no keys or paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ObjectStoreError {
    #[error("object store is unavailable")]
    Unavailable,
    #[error("object store denied the request")]
    Denied,
}

/// Private object storage. Implementations must never make objects publicly readable.
pub trait ObjectStore: Send + Sync {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), ObjectStoreError>;
    fn delete(&self, key: &str) -> Result<(), ObjectStoreError>;
    /// Every key in the store. Used only for orphan cleanup.
    fn keys(&self) -> Result<Vec<String>, ObjectStoreError>;
}

/// In-memory store for tests and CI. It never touches the network or disk.
#[derive(Debug, Default)]
pub struct MemoryObjectStore {
    objects: Mutex<HashMap<String, Vec<u8>>>,
    fail_next_puts: Mutex<u32>,
}

impl MemoryObjectStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes the next `n` `put` calls fail with [`ObjectStoreError::Unavailable`].
    pub fn fail_next_puts(&self, n: u32) {
        if let Ok(mut guard) = self.fail_next_puts.lock() {
            *guard = n;
        }
    }

    /// Returns the bytes stored under `key`, if any.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.objects.lock().ok()?.get(key).cloned()
    }

    pub fn len(&self) -> usize {
        self.objects.lock().map(|m| m.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl ObjectStore for MemoryObjectStore {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), ObjectStoreError> {
        {
            let mut fails = self
                .fail_next_puts
                .lock()
                .map_err(|_| ObjectStoreError::Unavailable)?;
            if *fails > 0 {
                *fails -= 1;
                return Err(ObjectStoreError::Unavailable);
            }
        }
        self.objects
            .lock()
            .map_err(|_| ObjectStoreError::Unavailable)?
            .insert(key.to_string(), bytes.to_vec());
        Ok(())
    }

    fn delete(&self, key: &str) -> Result<(), ObjectStoreError> {
        self.objects
            .lock()
            .map_err(|_| ObjectStoreError::Unavailable)?
            .remove(key);
        Ok(())
    }

    fn keys(&self) -> Result<Vec<String>, ObjectStoreError> {
        Ok(self
            .objects
            .lock()
            .map_err(|_| ObjectStoreError::Unavailable)?
            .keys()
            .cloned()
            .collect())
    }
}

/// Lifecycle of a stored enrollment photo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentStatus {
    /// The photo is stored and the record is active.
    Stored,
    /// The photo was deleted by expiry or revocation. The record remains for audit.
    Deleted,
}

/// Enrollment metadata. Contains no photo bytes and no template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrollmentRecord {
    pub enrollment_id: String,
    /// Opaque, random key. Never sent to browsers, and never placed in a public URL.
    pub object_key: String,
    /// Lowercase hex SHA-256 of the stored original bytes.
    pub content_sha256: String,
    pub byte_len: usize,
    pub width: u32,
    pub height: u32,
    pub policy_version: String,
    pub model_version: Option<String>,
    pub consent_reference: String,
    pub idempotency_key: Option<String>,
    pub status: EnrollmentStatus,
    /// Always `Pending` when Rust creates the record. Only Spring Boot can confirm it.
    pub persistence: PersistenceStatus,
    pub created_at_ms: u64,
}

/// Input for creating an enrollment. Borrowed, so the photo is not copied before validation.
#[derive(Debug, Clone, Copy)]
pub struct NewEnrollment<'a> {
    /// Original upload bytes, as received after ingress validation.
    pub bytes: &'a [u8],
    pub suitability: SuitabilityStatus,
    pub consent_reference: &'a str,
    pub policy_version: &'a str,
    pub model_version: Option<&'a str>,
    pub width: u32,
    pub height: u32,
    pub idempotency_key: Option<&'a str>,
    pub created_at_ms: u64,
}

/// Typed storage failures. None of them returns a false success.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StorageError {
    #[error("assessment did not pass, so the photo cannot be stored")]
    NotSuitable,
    #[error("request is invalid")]
    Validation(ValidationError),
    #[error("object storage failed")]
    Store(ObjectStoreError),
    #[error("idempotency key was already used for a different request")]
    IdempotencyConflict,
    #[error("enrollment was not found")]
    NotFound,
}

impl From<ValidationError> for StorageError {
    fn from(err: ValidationError) -> Self {
        StorageError::Validation(err)
    }
}

#[derive(Default)]
struct State {
    records: HashMap<String, EnrollmentRecord>,
    /// Maps an idempotency key to the enrollment it created.
    by_idempotency_key: HashMap<String, String>,
}

/// Creates, expires and cleans up enrollment photos through an [`ObjectStore`].
pub struct EnrollmentService<S: ObjectStore> {
    store: S,
    state: Mutex<State>,
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn new_object_key() -> String {
    format!("{OBJECT_KEY_PREFIX}{}", Uuid::new_v4().simple())
}

impl<S: ObjectStore> EnrollmentService<S> {
    pub fn new(store: S) -> Self {
        Self {
            store,
            state: Mutex::new(State::default()),
        }
    }

    /// Stores a passing, consented enrollment photo and records its metadata.
    ///
    /// A retry with the same idempotency key and the same bytes returns the existing record.
    /// The same key with different bytes or a different consent reference is a conflict.
    pub fn create(&self, input: NewEnrollment<'_>) -> Result<EnrollmentRecord, StorageError> {
        if input.suitability != SuitabilityStatus::AssessmentPassed {
            return Err(StorageError::NotSuitable);
        }
        validate_identifier("consentReference", input.consent_reference)?;
        validate_identifier("policyVersion", input.policy_version)?;
        if let Some(model) = input.model_version {
            validate_identifier("modelVersion", model)?;
        }
        if let Some(key) = input.idempotency_key {
            validate_identifier("idempotencyKey", key)?;
        }

        let content_sha256 = sha256_hex(input.bytes);

        if let Some(existing) = self.existing_for(&input, &content_sha256)? {
            return Ok(existing);
        }

        // The object is written before the record. A crash between the two leaves an orphan
        // that `sweep_orphans` removes. The reverse order could leave a record with no object.
        let object_key = new_object_key();
        self.store
            .put(&object_key, input.bytes)
            .map_err(StorageError::Store)?;

        let record = EnrollmentRecord {
            enrollment_id: Uuid::new_v4().simple().to_string(),
            object_key: object_key.clone(),
            content_sha256: content_sha256.clone(),
            byte_len: input.bytes.len(),
            width: input.width,
            height: input.height,
            policy_version: input.policy_version.to_string(),
            model_version: input.model_version.map(str::to_string),
            consent_reference: input.consent_reference.to_string(),
            idempotency_key: input.idempotency_key.map(str::to_string),
            status: EnrollmentStatus::Stored,
            persistence: PersistenceStatus::Pending,
            created_at_ms: input.created_at_ms,
        };

        let mut state = self.lock()?;
        // Another request with the same key may have finished while this one was writing.
        let raced = input.idempotency_key.and_then(|key| {
            state
                .by_idempotency_key
                .get(key)
                .and_then(|id| state.records.get(id))
                .cloned()
        });
        if let Some(existing) = raced {
            drop(state);
            let _ = self.store.delete(&object_key);
            return if existing.content_sha256 == content_sha256
                && existing.consent_reference == input.consent_reference
            {
                Ok(existing)
            } else {
                Err(StorageError::IdempotencyConflict)
            };
        }
        if let Some(key) = input.idempotency_key {
            state
                .by_idempotency_key
                .insert(key.to_string(), record.enrollment_id.clone());
        }
        state
            .records
            .insert(record.enrollment_id.clone(), record.clone());
        Ok(record)
    }

    /// Looks up an existing record for an idempotency key, returning it only when the request matches.
    fn existing_for(
        &self,
        input: &NewEnrollment<'_>,
        content_sha256: &str,
    ) -> Result<Option<EnrollmentRecord>, StorageError> {
        let Some(key) = input.idempotency_key else {
            return Ok(None);
        };
        let state = self.lock()?;
        let Some(id) = state.by_idempotency_key.get(key) else {
            return Ok(None);
        };
        let Some(existing) = state.records.get(id) else {
            return Ok(None);
        };
        if existing.content_sha256 == content_sha256
            && existing.consent_reference == input.consent_reference
        {
            Ok(Some(existing.clone()))
        } else {
            Err(StorageError::IdempotencyConflict)
        }
    }

    pub fn get(&self, enrollment_id: &str) -> Result<EnrollmentRecord, StorageError> {
        self.lock()?
            .records
            .get(enrollment_id)
            .cloned()
            .ok_or(StorageError::NotFound)
    }

    /// Deletes the photo and marks the record `Deleted`. Used for revocation and expiry.
    /// If the delete fails, the record stays `Stored`, so the call can be retried.
    pub fn expire(&self, enrollment_id: &str) -> Result<EnrollmentRecord, StorageError> {
        let record = self.get(enrollment_id)?;
        if record.status == EnrollmentStatus::Deleted {
            return Ok(record);
        }
        self.store
            .delete(&record.object_key)
            .map_err(StorageError::Store)?;
        let mut state = self.lock()?;
        let entry = state
            .records
            .get_mut(enrollment_id)
            .ok_or(StorageError::NotFound)?;
        entry.status = EnrollmentStatus::Deleted;
        Ok(entry.clone())
    }

    /// Deletes objects under this service's prefix that no `Stored` record refers to.
    /// Returns the number of objects removed. Stops at the first failure and reports it.
    pub fn sweep_orphans(&self) -> Result<usize, StorageError> {
        let referenced: std::collections::HashSet<String> = self
            .lock()?
            .records
            .values()
            .filter(|r| r.status == EnrollmentStatus::Stored)
            .map(|r| r.object_key.clone())
            .collect();
        let keys = self.store.keys().map_err(StorageError::Store)?;
        let mut removed = 0;
        for key in keys {
            if !key.starts_with(OBJECT_KEY_PREFIX) || referenced.contains(&key) {
                continue;
            }
            self.store.delete(&key).map_err(StorageError::Store)?;
            removed += 1;
        }
        Ok(removed)
    }

    /// Access to the underlying store, for tests and the S3 adapter's own checks.
    pub fn store(&self) -> &S {
        &self.store
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, State>, StorageError> {
        self.state
            .lock()
            .map_err(|_| StorageError::Store(ObjectStoreError::Unavailable))
    }
}
