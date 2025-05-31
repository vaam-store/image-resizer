use crate::services::storage::core::StorageBackend;
use anyhow::{Result, anyhow};
use derive_builder::Builder;
use std::env;
use std::sync::Arc;

/// Factory for creating storage backends based on configuration
#[derive(Clone, Builder)]
pub struct StorageService {
    storage: Arc<dyn StorageBackend>,
    cdn_base_url: String,
}

/// Storage type options
#[derive(Debug, Clone, PartialEq)]
pub enum StorageType {
    S3,
    LocalFs,
    InMemory,
}

impl StorageType {
    /// Parse storage type from string
    pub fn from_str(s: &str) -> Result<Self> {
        match s.to_uppercase().as_str() {
            "S3" | "MINIO" => Ok(StorageType::S3),
            "LOCAL_FS" | "LOCALFS" | "LOCAL" => Ok(StorageType::LocalFs),
            "IN_MEMORY" | "INMEMORY" | "MEMORY" => Ok(StorageType::InMemory),
            _ => Err(anyhow!("Invalid storage type: {}", s)),
        }
    }
}

impl StorageService {
    /// Create a new storage backend based on configuration
    ///
    /// This is the unified method to create storage backends.
    /// If multiple storage features are enabled, the choice is made via the
    /// environment variable "storage_type". If only one storage feature is enabled,
    /// it is used automatically.
    pub fn new(config: StorageConfig) -> Result<Self> {
        // Determine which storage type to use
        let storage_type = Self::determine_storage_type(config.storage_type)?;

        match storage_type {
            #[cfg(feature = "s3")]
            StorageType::S3 => Self::create_s3_storage(
                config
                    .s3_config
                    .ok_or_else(|| anyhow!("S3 configuration is required"))?,
                config.cdn_base_url,
            ),

            #[cfg(feature = "local_fs")]
            StorageType::LocalFs => Self::create_local_fs_storage(
                config
                    .local_fs_config
                    .ok_or_else(|| anyhow!("Local FS configuration is required"))?,
                config.cdn_base_url,
            ),

            #[cfg(feature = "in_memory")]
            StorageType::InMemory => Self::create_in_memory_storage(config.cdn_base_url),

            #[allow(unreachable_patterns)]
            _ => Err(anyhow!(
                "No storage backend available for the selected type"
            )),
        }
    }

    /// Determine which storage type to use based on enabled features and configuration
    fn determine_storage_type(storage_type_str: Option<String>) -> Result<StorageType> {
        // Count enabled storage features
        let mut enabled_features = 0;

        #[cfg(feature = "s3")]
        {
            enabled_features += 1;
        }

        #[cfg(feature = "local_fs")]
        {
            enabled_features += 1;
        }

        #[cfg(feature = "in_memory")]
        {
            enabled_features += 1;
        }

        // If no features are enabled, return an error
        if enabled_features == 0 {
            return Err(anyhow!("No storage features are enabled"));
        }

        // If only one feature is enabled, use it
        if enabled_features == 1 {
            #[cfg(feature = "s3")]
            return Ok(StorageType::S3);

            #[cfg(feature = "local_fs")]
            return Ok(StorageType::LocalFs);

            #[cfg(feature = "in_memory")]
            return Ok(StorageType::InMemory);
        }

        // If multiple features are enabled, use the storage_type parameter or environment variable
        if let Some(storage_type) = storage_type_str {
            return StorageType::from_str(&storage_type);
        }

        // Try to get from environment variable
        if let Ok(storage_type) = env::var("STORAGE_TYPE") {
            return StorageType::from_str(&storage_type);
        }

        // Default to the first available storage type
        #[cfg(feature = "s3")]
        return Ok(StorageType::S3);

        #[cfg(feature = "local_fs")]
        return Ok(StorageType::LocalFs);

        #[cfg(feature = "in_memory")]
        return Ok(StorageType::InMemory);

        // This code is unreachable due to the checks above, but kept for completeness
        #[allow(unreachable_code)]
        Err(anyhow!("No storage features are enabled"))
    }

    /// Create a new MinIO storage backend
    #[cfg(feature = "s3")]
    fn create_s3_storage(config: S3Config, cdn_base_url: String) -> Result<Self> {
        let s3_storage_adapter = crate::services::storage::s3_handler::MinIOStorage::new_minio(
            config.endpoint_url,
            config.access_key,
            config.secret_key,
            config.bucket,
            config.region,
        )?;

        Ok(Self {
            storage: Arc::new(s3_storage_adapter),
            cdn_base_url,
        })
    }

    /// Create a new local file system storage backend
    #[cfg(feature = "local_fs")]
    fn create_local_fs_storage(config: LocalFsConfig, cdn_base_url: String) -> Result<Self> {
        let local_fs_storage_adapter =
            crate::services::storage::local_fs_handler::LocalFSStorage::new(config.base_path)?;

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
    fn create_in_memory_storage(cdn_base_url: String) -> Result<Self> {
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

    /// Check if an image exists in the cache
    pub async fn check_cache(&self, key: &str) -> Result<bool> {
        self.storage.check_cache(key).await
    }

    /// Get the CDN URL for a cached image
    pub fn get_cdn_url(&self, key: &str) -> String {
        format!("{}/{}", self.cdn_base_url.trim_end_matches('/'), key)
    }

    /// Get an image from storage
    pub async fn get_image(&self, key: &str) -> Result<Vec<u8>> {
        self.storage.get_image(key).await
    }
}

/// Configuration for S3 storage
#[derive(Debug, Clone)]
pub struct S3Config {
    #[allow(dead_code)]
    pub endpoint_url: String,
    #[allow(dead_code)]
    pub access_key: String,
    #[allow(dead_code)]
    pub secret_key: String,
    #[allow(dead_code)]
    pub bucket: String,
    #[allow(dead_code)]
    pub region: String,
}

/// Configuration for local file system storage
#[derive(Debug, Clone)]
pub struct LocalFsConfig {
    pub base_path: std::path::PathBuf,
}

/// Configuration for storage service
#[derive(Debug, Clone, Default)]
pub struct StorageConfig {
    pub storage_type: Option<String>,
    pub cdn_base_url: String,
    #[allow(dead_code)]
    pub s3_config: Option<S3Config>,
    pub local_fs_config: Option<LocalFsConfig>,
}

impl StorageConfig {
    /// Create a new storage configuration
    pub fn new(cdn_base_url: String) -> Self {
        Self {
            storage_type: None,
            cdn_base_url,
            s3_config: None,
            local_fs_config: None,
        }
    }

    /// Set the storage type
    pub fn with_storage_type(mut self, storage_type: impl Into<String>) -> Self {
        self.storage_type = Some(storage_type.into());
        self
    }

    /// Set the S3 configuration
    #[allow(dead_code)]
    pub fn with_s3_config(
        mut self,
        endpoint_url: String,
        access_key: String,
        secret_key: String,
        bucket: String,
        region: String,
    ) -> Self {
        self.s3_config = Some(S3Config {
            endpoint_url,
            access_key,
            secret_key,
            bucket,
            region,
        });
        self
    }

    /// Set the local file system configuration
    pub fn with_local_fs_config(mut self, base_path: impl AsRef<std::path::Path>) -> Self {
        self.local_fs_config = Some(LocalFsConfig {
            base_path: base_path.as_ref().to_path_buf(),
        });
        self
    }
}
