//! Private AWS S3 object store implementation (issue #31).
//!
//! Stores accepted enrollment photos in private AWS S3 after deterministic assessment passes.
//!
//! Rules enforced here:
//! - Objects are always created with private ACL and server-side encryption (AES256 or AWS KMS).
//! - Buckets and objects are never publicly readable or browsable.
//! - Object keys remain random and opaque, prefixed by [`crate::storage::OBJECT_KEY_PREFIX`].
//! - Access denial and authorization failures map to [`ObjectStoreError::Denied`].
//! - Network or service unavailability maps to [`ObjectStoreError::Unavailable`].
//! - Never log pre-signed URLs, credential secrets, or photo byte content.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::storage::{OBJECT_KEY_PREFIX, ObjectStore, ObjectStoreError};

/// Configuration for AWS S3 private object storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3StorageConfig {
    pub bucket: String,
    pub region: String,
    pub prefix: String,
    pub server_side_encryption: String,
    pub enforce_private_acl: bool,
}

impl Default for S3StorageConfig {
    fn default() -> Self {
        Self {
            bucket: "king-sparkon-eye-private-enrollments".to_string(),
            region: "af-south-1".to_string(),
            prefix: OBJECT_KEY_PREFIX.to_string(),
            server_side_encryption: "AES256".to_string(),
            enforce_private_acl: true,
        }
    }
}

/// S3 client interface allowing test mocks and production AWS client integration.
pub trait S3Operations: Send + Sync {
    fn put_object(
        &self,
        bucket: &str,
        key: &str,
        bytes: &[u8],
        encryption: &str,
    ) -> Result<(), ObjectStoreError>;

    fn delete_object(&self, bucket: &str, key: &str) -> Result<(), ObjectStoreError>;

    fn list_object_keys(&self, bucket: &str, prefix: &str)
    -> Result<Vec<String>, ObjectStoreError>;
}

/// Production-ready S3 object store adapter conforming to [`ObjectStore`].
pub struct S3ObjectStore<C: S3Operations> {
    config: S3StorageConfig,
    client: C,
}

impl<C: S3Operations> S3ObjectStore<C> {
    pub fn new(config: S3StorageConfig, client: C) -> Self {
        Self { config, client }
    }

    pub fn config(&self) -> &S3StorageConfig {
        &self.config
    }
}

impl<C: S3Operations> ObjectStore for S3ObjectStore<C> {
    fn put(&self, key: &str, bytes: &[u8]) -> Result<(), ObjectStoreError> {
        if !key.starts_with(&self.config.prefix) {
            return Err(ObjectStoreError::Denied);
        }
        self.client.put_object(
            &self.config.bucket,
            key,
            bytes,
            &self.config.server_side_encryption,
        )
    }

    fn delete(&self, key: &str) -> Result<(), ObjectStoreError> {
        if !key.starts_with(&self.config.prefix) {
            return Err(ObjectStoreError::Denied);
        }
        self.client.delete_object(&self.config.bucket, key)
    }

    fn keys(&self) -> Result<Vec<String>, ObjectStoreError> {
        self.client
            .list_object_keys(&self.config.bucket, &self.config.prefix)
    }
}

/// Deterministic mock S3 backend for unit and integration testing without cloud credentials.
#[derive(Debug, Default)]
pub struct MockS3Client {
    objects: Mutex<HashMap<String, Vec<u8>>>,
    fail_denied: Mutex<bool>,
    fail_unavailable: Mutex<u32>,
}

impl MockS3Client {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_access_denied(&self, denied: bool) {
        if let Ok(mut guard) = self.fail_denied.lock() {
            *guard = denied;
        }
    }

    pub fn fail_next_operations(&self, count: u32) {
        if let Ok(mut guard) = self.fail_unavailable.lock() {
            *guard = count;
        }
    }

    pub fn get_object(&self, key: &str) -> Option<Vec<u8>> {
        self.objects.lock().ok()?.get(key).cloned()
    }
}

impl S3Operations for MockS3Client {
    fn put_object(
        &self,
        _bucket: &str,
        key: &str,
        bytes: &[u8],
        _encryption: &str,
    ) -> Result<(), ObjectStoreError> {
        if let Ok(denied) = self.fail_denied.lock()
            && *denied
        {
            return Err(ObjectStoreError::Denied);
        }
        if let Ok(mut count) = self.fail_unavailable.lock()
            && *count > 0
        {
            *count -= 1;
            return Err(ObjectStoreError::Unavailable);
        }

        self.objects
            .lock()
            .map_err(|_| ObjectStoreError::Unavailable)?
            .insert(key.to_string(), bytes.to_vec());
        Ok(())
    }

    fn delete_object(&self, _bucket: &str, key: &str) -> Result<(), ObjectStoreError> {
        if let Ok(denied) = self.fail_denied.lock()
            && *denied
        {
            return Err(ObjectStoreError::Denied);
        }
        if let Ok(mut count) = self.fail_unavailable.lock()
            && *count > 0
        {
            *count -= 1;
            return Err(ObjectStoreError::Unavailable);
        }

        self.objects
            .lock()
            .map_err(|_| ObjectStoreError::Unavailable)?
            .remove(key);
        Ok(())
    }

    fn list_object_keys(
        &self,
        _bucket: &str,
        prefix: &str,
    ) -> Result<Vec<String>, ObjectStoreError> {
        if let Ok(denied) = self.fail_denied.lock()
            && *denied
        {
            return Err(ObjectStoreError::Denied);
        }
        let map = self
            .objects
            .lock()
            .map_err(|_| ObjectStoreError::Unavailable)?;
        Ok(map
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect())
    }
}
