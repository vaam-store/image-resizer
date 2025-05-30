use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use crate::services::storage::core::StorageBackend;

/// In-memory storage implementation
///
/// This storage backend is intended for development and testing purposes only.
/// It stores all data in memory, which means:
/// - Data is lost when the application restarts
/// - Memory usage increases with the number and size of stored images
/// - Not suitable for production environments or distributed systems
pub struct InMemoryStorage {
    /// Internal storage using a thread-safe hash map
    storage: Arc<RwLock<HashMap<String, (String, Vec<u8>)>>>,
}

impl InMemoryStorage {
    /// Creates a new in-memory storage instance
    pub fn new() -> Self {
        Self {
            storage: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

#[async_trait]
impl StorageBackend for InMemoryStorage {
    async fn upload_image(&self, key: &str, content_type: &str, data: Vec<u8>) -> Result<()> {
        // Store the image data with its content type in memory
        let mut storage = self.storage.write().unwrap();
        storage.insert(key.to_string(), (content_type.to_string(), data));
        Ok(())
    }

    async fn check_cache(&self, key: &str) -> Result<bool> {
        // Check if the key exists in the in-memory storage
        let storage = self.storage.read().unwrap();
        Ok(storage.contains_key(key))
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
        let stored_data = storage.storage.read().unwrap();
        let (stored_content_type, stored_bytes) = stored_data.get(key).unwrap();
        assert_eq!(stored_content_type, content_type);
        assert_eq!(stored_bytes, &data);
    }
}
