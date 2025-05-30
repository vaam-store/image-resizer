use crate::services::storage::core::StorageBackend;
use anyhow::Result;
use derive_builder::Builder;
use std::path::Path;
use std::sync::Arc;

/// Factory for creating storage backends based on configuration
#[derive(Clone, Builder)]
pub struct StorageService {
    storage: Arc<dyn StorageBackend>,
    cdn_base_url: String,
}

impl StorageService {
    /// Create a new MinIO storage backend
    #[cfg(feature = "s3")]
    pub fn new_minio(
        endpoint_url: String,
        access_key: String,
        secret_key: String,
        bucket: String,
        region: String,
        cdn_base_url: String,
    ) -> Result<Self> {
        let s3_storage_adapter = crate::services::storage::s3_handler::MinIOStorage::new_minio(
            endpoint_url,
            access_key,
            secret_key,
            bucket,
            region,
        )?;

        Ok(Self {
            storage: Arc::new(s3_storage_adapter),
            cdn_base_url,
        })
    }

    /// Create a new local file system storage backend
    #[cfg(feature = "local_fs")]
    pub fn new_local_fs(base_path: impl AsRef<Path>, cdn_base_url: String) -> Result<Self> {
        let local_fs_storage_adapter =
            crate::services::storage::local_fs_handler::LocalFSStorage::new(
                base_path.as_ref().to_path_buf(),
            )?;

        Ok(Self {
            storage: Arc::new(local_fs_storage_adapter),
            cdn_base_url,
        })
    }

    /// Create a new in-memory storage backend
    ///
    /// # Note
    /// This storage backend is intended for development and testing purposes only.
    /// Data is stored in memory and will be lost when the application restarts.
    #[cfg(feature = "in_memory")]
    pub fn new_in_memory(cdn_base_url: String) -> Result<Self> {
        let in_memory_storage_adapter =
            crate::services::storage::in_memory_handler::InMemoryStorage::new();

        Ok(Self {
            storage: Arc::new(in_memory_storage_adapter),
            cdn_base_url,
        })
    }

    /// Upload an image to storage
    pub async fn upload_image(&self, key: &str, content_type: &str, data: Vec<u8>) -> Result<()> {
        self.storage.upload_image(key, content_type, data).await
    }

    /// Check if an image exists in cache
    pub async fn check_cache(&self, key: &str) -> Result<bool> {
        self.storage.check_cache(key).await
    }

    /// Get the CDN URL for a cached image
    pub fn get_cdn_url(&self, key: &str) -> String {
        format!("{}/{}", self.cdn_base_url.trim_end_matches('/'), key)
    }
}
