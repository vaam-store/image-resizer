use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use crate::services::storage::core::StorageBackend;

/// One stored entry: content type, bytes, and an optional expiry (#40).
type Entry = (String, Vec<u8>, Option<Instant>);

/// In-memory storage implementation.
///
/// # Not reachable in a release build (#39)
///
/// This backend has no TTL enforcement of its own memory footprint, no LRU,
/// no entry cap and no byte cap - every distinct parameter combination is a
/// permanent allocation until restart. That is fine for what it is actually
/// for (exercising `StorageBackend` in this crate's own test suite without
/// standing up a filesystem or S3/MinIO), but `STORAGE_TYPE=IN_MEMORY`
/// selecting it in an operator's real deployment would mean unbounded RSS
/// growth with no cap - see #39.
///
/// Rather than half-fixing that by bolting on an LRU (which this backend,
/// as pure test scaffolding, has no real need for), this type - and every
/// path that can construct or select it (`StorageService::create_in_memory_storage`,
/// its `StorageType::InMemory` match arm) - is gated `#[cfg(all(test, feature =
/// "in_memory"))]`: it simply does not exist in a non-test build, regardless
/// of whether the `in_memory` Cargo feature is enabled. Selecting
/// `STORAGE_TYPE=IN_MEMORY` in a release build now fails fast at startup
/// with "No storage backend available for the selected type" instead of
/// silently running an unbounded, lock-poisoning cache in production.
pub struct InMemoryStorage {
    /// Internal storage using a thread-safe hash map.
    storage: Arc<RwLock<HashMap<String, Entry>>>,
}

impl InMemoryStorage {
    /// Creates a new in-memory storage instance
    pub fn new() -> Self {
        Self {
            storage: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Read-lock helper that recovers from a poisoned lock instead of
    /// panicking (#39): a panic while holding the lock (e.g. inside a test)
    /// must not permanently break every later call to this backend. Lost
    /// updates from the panicking writer are an acceptable trade-off for a
    /// test-only backend - the alternative (`.unwrap()`) is a permanent
    /// outage from a single panic.
    fn read_storage(&self) -> std::sync::RwLockReadGuard<'_, HashMap<String, Entry>> {
        self.storage
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Write-side counterpart of [`Self::read_storage`].
    fn write_storage(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<String, Entry>> {
        self.storage
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[async_trait]
impl StorageBackend for InMemoryStorage {
    async fn upload_image_with_ttl(
        &self,
        key: &str,
        content_type: &str,
        data: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<()> {
        let expires_at = ttl.and_then(|ttl| Instant::now().checked_add(ttl));
        let mut storage = self.write_storage();
        storage.insert(
            key.to_string(),
            (content_type.to_string(), data, expires_at),
        );
        Ok(())
    }

    async fn check_cache(&self, key: &str) -> Result<bool> {
        let storage = self.read_storage();
        match storage.get(key) {
            Some((_, _, Some(expires_at))) if Instant::now() >= *expires_at => Ok(false),
            Some(_) => Ok(true),
            None => Ok(false),
        }
    }

    async fn get_image(&self, key: &str) -> Result<Vec<u8>> {
        let storage = self.read_storage();
        match storage.get(key) {
            Some((_, _, Some(expires_at))) if Instant::now() >= *expires_at => Err(
                anyhow::anyhow!("Image not found in memory storage: {} (expired)", key),
            ),
            Some((_, data, _)) => Ok(data.clone()),
            None => Err(anyhow::anyhow!(
                "Image not found in memory storage: {}",
                key
            )),
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        // Idempotent (#40): removing an absent key is still `Ok(())`.
        let mut storage = self.write_storage();
        storage.remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_storage() {
        let storage = InMemoryStorage::new();

        // Test uploading an image
        let key = "test-image.jpg";
        let content_type = "image/jpeg";
        let data = vec![1, 2, 3, 4, 5]; // Dummy image data

        assert!(
            storage
                .upload_image(key, content_type, data.clone())
                .await
                .is_ok()
        );

        // Test checking cache
        assert!(storage.check_cache(key).await.unwrap());
        assert!(!storage.check_cache("nonexistent-key").await.unwrap());

        // Verify the stored data
        let stored_data = storage.read_storage();
        let (stored_content_type, stored_bytes, expires_at) = stored_data.get(key).unwrap();
        assert_eq!(stored_content_type, content_type);
        assert_eq!(stored_bytes, &data);
        assert!(expires_at.is_none(), "no TTL was set for this upload");
        drop(stored_data);

        // Test deletion (#40): idempotent, and check_cache reports it gone.
        storage.delete(key).await.expect("delete should succeed");
        assert!(!storage.check_cache(key).await.unwrap());
        storage
            .delete(key)
            .await
            .expect("deleting an already-absent key should still succeed");
        storage
            .delete("nonexistent-key")
            .await
            .expect("deleting a never-existed key should still succeed");
    }

    /// A TTL'd entry must be reported absent by both `check_cache` and
    /// `get_image` once it has expired (#40), without needing a background
    /// sweep - expiry is checked lazily, on read.
    #[tokio::test]
    async fn ttl_expired_entry_is_reported_absent() {
        let storage = InMemoryStorage::new();
        let key = "ttl-key.jpg";

        storage
            .upload_image_with_ttl(key, "image/jpeg", vec![1, 2, 3], Some(Duration::ZERO))
            .await
            .expect("upload with a zero TTL should succeed");

        // A zero-duration TTL has already elapsed by the time we check.
        tokio::time::sleep(Duration::from_millis(1)).await;

        assert!(!storage.check_cache(key).await.unwrap());
        assert!(storage.get_image(key).await.is_err());
    }

    /// An entry uploaded with a TTL far in the future must still be served
    /// normally - the mechanism must not evict early.
    #[tokio::test]
    async fn ttl_not_yet_expired_entry_is_served() {
        let storage = InMemoryStorage::new();
        let key = "ttl-future-key.jpg";
        let data = vec![9, 9, 9];

        storage
            .upload_image_with_ttl(
                key,
                "image/jpeg",
                data.clone(),
                Some(Duration::from_secs(3600)),
            )
            .await
            .expect("upload with a future TTL should succeed");

        assert!(storage.check_cache(key).await.unwrap());
        assert_eq!(storage.get_image(key).await.unwrap(), data);
    }
}
