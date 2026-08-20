use crate::services::storage::core::StorageBackend;
use anyhow::{Result, anyhow};
use derive_builder::Builder;
use std::env;
use std::sync::Arc;
use std::time::Duration;

/// Factory for creating storage backends based on configuration
#[derive(Clone, Builder)]
pub struct StorageService {
    storage: Arc<dyn StorageBackend>,
    cdn_base_url: String,
    /// The `STORAGE_SUB_PATH` prefix `CacheService::generate_key` prepends
    /// before `<hash>.<ext>`. Used to validate that a `key` reaching this
    /// service matches exactly what `generate_key` can produce (#23) before
    /// it is passed to any backend. Defaults to `""` (`StorageConfig::new`'s
    /// default), matching this repo's `STORAGE_SUB_PATH` default - see
    /// `StorageConfig::with_key_prefix` if that default is overridden.
    key_prefix: String,
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
                config.key_prefix,
            ),

            #[cfg(feature = "local_fs")]
            StorageType::LocalFs => Self::create_local_fs_storage(
                config
                    .local_fs_config
                    .ok_or_else(|| anyhow!("Local FS configuration is required"))?,
                config.cdn_base_url,
                config.key_prefix,
            ),

            // Not reachable outside this crate's own test builds (#39) - see
            // `InMemoryStorage`'s doc comment. In a release build (even one
            // compiled with the `in_memory` Cargo feature on) this arm does
            // not exist, so selecting `STORAGE_TYPE=IN_MEMORY` there falls
            // through to the catch-all `Err` below instead of running an
            // unbounded, lock-poisoning cache in production.
            #[cfg(all(test, feature = "in_memory"))]
            StorageType::InMemory => {
                Self::create_in_memory_storage(config.cdn_base_url, config.key_prefix)
            }

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
    fn create_s3_storage(
        config: S3Config,
        cdn_base_url: String,
        key_prefix: String,
    ) -> Result<Self> {
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
            key_prefix,
        })
    }

    /// Create a new local file system storage backend
    #[cfg(feature = "local_fs")]
    fn create_local_fs_storage(
        config: LocalFsConfig,
        cdn_base_url: String,
        key_prefix: String,
    ) -> Result<Self> {
        let local_fs_storage_adapter =
            crate::services::storage::local_fs_handler::LocalFSStorage::new(config.base_path)?;

        Ok(Self {
            storage: Arc::new(local_fs_storage_adapter),
            cdn_base_url,
            key_prefix,
        })
    }

    /// Create a new in-memory storage backend
    ///
    /// # Note
    /// This storage backend exists only for this crate's own test builds
    /// (#39) - see `InMemoryStorage`'s doc comment for why it is not
    /// reachable from a release build regardless of Cargo feature flags.
    #[cfg(all(test, feature = "in_memory"))]
    fn create_in_memory_storage(cdn_base_url: String, key_prefix: String) -> Result<Self> {
        let in_memory_storage_adapter =
            crate::services::storage::in_memory_handler::InMemoryStorage::new();

        Ok(Self {
            storage: Arc::new(in_memory_storage_adapter),
            cdn_base_url,
            key_prefix,
        })
    }

    /// Upload an image to storage.
    ///
    /// Validates `key` against exactly the shape `CacheService::generate_key`
    /// produces before it ever reaches a backend (#23) - protects every
    /// backend (S3, local_fs, in-memory) from a single choke point instead
    /// of relying on each implementation to defend itself.
    pub async fn upload_image(&self, key: &str, content_type: &str, data: Vec<u8>) -> Result<()> {
        crate::services::storage::key_validation::validate_cache_key(key, &self.key_prefix)?;
        self.storage.upload_image(key, content_type, data).await
    }

    /// Upload an image to storage with an expiry (#40). See
    /// [`Self::upload_image`] for the key-validation rationale; see
    /// [`StorageBackend::upload_image_with_ttl`] for what `ttl` means.
    pub async fn upload_image_with_ttl(
        &self,
        key: &str,
        content_type: &str,
        data: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<()> {
        crate::services::storage::key_validation::validate_cache_key(key, &self.key_prefix)?;
        self.storage
            .upload_image_with_ttl(key, content_type, data, ttl)
            .await
    }

    /// Deletes a cached object, purging it from storage (#40). Same key
    /// validation as every other entry point (#23) - a delete call is just
    /// as dangerous an arbitrary-file/IDOR primitive as a read would be if
    /// an unvalidated key reached a backend. Idempotent: deleting an
    /// already-absent key still succeeds.
    ///
    /// # Note
    /// Not yet exposed over HTTP - a purge endpoint needs authentication
    /// (#27, not built yet). This exists at the service layer so it is
    /// ready to wire up as soon as #27 lands, instead of leaving
    /// `StorageBackend` itself without the operation in the meantime.
    pub async fn delete(&self, key: &str) -> Result<()> {
        crate::services::storage::key_validation::validate_cache_key(key, &self.key_prefix)?;
        self.storage.delete(key).await
    }

    /// Check if an image exists in the cache. See [`Self::upload_image`] for
    /// why `key` is validated here, before touching any backend.
    pub async fn check_cache(&self, key: &str) -> Result<bool> {
        crate::services::storage::key_validation::validate_cache_key(key, &self.key_prefix)?;
        self.storage.check_cache(key).await
    }

    /// Get the CDN URL for a cached image
    pub fn get_cdn_url(&self, key: &str) -> String {
        format!("{}/{}", self.cdn_base_url.trim_end_matches('/'), key)
    }

    /// Get an image from storage. See [`Self::upload_image`] for why `key`
    /// is validated here, before touching any backend - this is the
    /// arbitrary-file-read path (#23): an unvalidated key here is either a
    /// local path traversal or an S3 IDOR across the whole bucket.
    pub async fn get_image(&self, key: &str) -> Result<Vec<u8>> {
        crate::services::storage::key_validation::validate_cache_key(key, &self.key_prefix)?;
        self.storage.get_image(key).await
    }
}

/// Configuration for S3 storage
#[derive(Debug, Clone)]
#[cfg(feature = "s3")]
pub struct S3Config {
    pub endpoint_url: String,
    pub access_key: String,
    pub secret_key: String,
    pub bucket: String,
    pub region: String,
}

/// Configuration for local file system storage
#[derive(Debug, Clone)]
pub struct LocalFsConfig {
    pub base_path: std::path::PathBuf,
}

/// Configuration for storage service
#[derive(Debug, Clone, Default)]
#[cfg(feature = "s3")]
pub struct StorageConfig {
    pub storage_type: Option<String>,
    pub cdn_base_url: String,
    pub s3_config: Option<S3Config>,
    pub local_fs_config: Option<LocalFsConfig>,
    /// The `STORAGE_SUB_PATH` prefix `CacheService::generate_key` prepends
    /// before `<hash>.<ext>`. Defaults to `""`, matching this repo's
    /// `STORAGE_SUB_PATH` env default - see `with_key_prefix`.
    pub key_prefix: String,
}

#[cfg(not(feature = "s3"))]
pub struct StorageConfig {
    pub storage_type: Option<String>,
    pub cdn_base_url: String,
    pub local_fs_config: Option<LocalFsConfig>,
    /// The `STORAGE_SUB_PATH` prefix `CacheService::generate_key` prepends
    /// before `<hash>.<ext>`. Defaults to `""`, matching this repo's
    /// `STORAGE_SUB_PATH` env default - see `with_key_prefix`.
    pub key_prefix: String,
}

#[cfg(feature = "s3")]
impl StorageConfig {
    /// Create a new storage configuration
    pub fn new(cdn_base_url: String) -> Self {
        Self {
            storage_type: None,
            cdn_base_url,
            s3_config: None,
            local_fs_config: None,
            key_prefix: String::new(),
        }
    }
}

#[cfg(not(feature = "s3"))]
impl StorageConfig {
    /// Create a new storage configuration
    pub fn new(cdn_base_url: String) -> Self {
        Self {
            storage_type: None,
            cdn_base_url,
            local_fs_config: None,
            key_prefix: String::new(),
        }
    }
}

// Common implementation for StorageConfig
impl StorageConfig {
    /// Set the storage type
    pub fn with_storage_type(mut self, storage_type: impl Into<String>) -> Self {
        self.storage_type = Some(storage_type.into());
        self
    }

    /// Set the cache-key prefix used to validate `key` before it reaches any
    /// storage backend (#23). Must match whatever `STORAGE_SUB_PATH` the
    /// `CacheService` in front of this `StorageService` is configured with
    /// (`CacheServiceBuilder::minio_sub_path`), or legitimately-generated
    /// keys will be rejected as invalid. Defaults to `""`.
    pub fn with_key_prefix(mut self, key_prefix: impl Into<String>) -> Self {
        self.key_prefix = key_prefix.into();
        self
    }

    /// Set the S3 configuration
    #[cfg(feature = "s3")]
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
    #[cfg(feature = "local_fs")]
    pub fn with_local_fs_config(mut self, base_path: impl AsRef<std::path::Path>) -> Self {
        self.local_fs_config = Some(LocalFsConfig {
            base_path: base_path.as_ref().to_path_buf(),
        });
        self
    }
}

#[cfg(all(test, feature = "local_fs"))]
mod tests {
    use super::*;
    use crate::models::params::ResizeQuery;
    use crate::services::cache::handler::CacheServiceBuilder;
    // #53: `gen_server` (OpenAPI codegen) was deleted; `ImageFormat` is now
    // hand-written in `src/models/params.rs`. Mechanical import change
    // only - no logic here changed.
    use crate::models::params::{ImageFormat, ResizeType};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// Owns a per-test local_fs storage directory under the OS temp dir and
    /// removes it on drop, so repeated test runs don't litter the temp dir.
    /// Mirrors `TestStorageDir` in `src/modules/api/resize.rs`.
    struct TestStorageDir(std::path::PathBuf);

    impl std::ops::Deref for TestStorageDir {
        type Target = std::path::Path;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl Drop for TestStorageDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_storage_dir() -> TestStorageDir {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = TestStorageDir(std::env::temp_dir().join(format!(
            "emgr-storage-handler-test-{}-{}",
            std::process::id(),
            id
        )));
        std::fs::create_dir_all(&*dir).expect("create test storage dir");
        dir
    }

    /// A non-empty `STORAGE_SUB_PATH` must round-trip: a key generated by
    /// `CacheService` (with `minio_sub_path` set) has to be accepted by
    /// `StorageService::{check_cache,upload_image,get_image}` when
    /// `StorageConfig` is given the *same* prefix via `with_key_prefix`.
    ///
    /// This is exactly the wiring `ApiService::create`
    /// (`src/modules/api/handler.rs`) performs for real. Before that wiring
    /// landed, `StorageConfig::key_prefix` silently defaulted to `""`
    /// regardless of `STORAGE_SUB_PATH`, so `CacheService::generate_key`
    /// would emit `{sub_path}{hash}.{ext}` while the storage validator only
    /// ever accepted `{hash}.{ext}` - rejecting every legitimately-generated
    /// key outright as soon as an operator configured a non-default
    /// sub-path. Deployments that leave `STORAGE_SUB_PATH` at its default
    /// empty string never observed this, which is why nothing caught it.
    #[tokio::test]
    async fn non_empty_sub_path_round_trips_through_check_cache_and_get_image() {
        let dir = test_storage_dir();
        let sub_path = "sub/";

        let storage_config = StorageConfig::new("http://cdn.test".to_string())
            .with_key_prefix(sub_path)
            .with_local_fs_config(&*dir);
        let storage = StorageService::new(storage_config).expect("build storage service");

        let cache = CacheServiceBuilder::default()
            .minio_sub_path(sub_path.to_string())
            .build()
            .expect("build cache service");

        let params = ResizeQuery {
            url: "https://example.com/img.png".to_string(),
            width: Some(100),
            height: Some(100),
            resize_type: ResizeType::Fit,
            format: ImageFormat::Png,
            blur_sigma: None,
            grayscale: None,
            enlarge: false,
            quality: None,
            background: None,
        };
        let key = cache.generate_key(&params);
        assert!(
            key.starts_with(sub_path),
            "expected generated key '{key}' to start with the configured sub_path '{sub_path}'"
        );

        // Not uploaded yet - check_cache must accept the key shape (not
        // reject it as invalid, which would have surfaced as an `Err` here
        // before the sub_path was wired in) and correctly report it absent.
        assert!(
            !storage
                .check_cache(&key)
                .await
                .expect("check_cache should accept a validly-prefixed key"),
            "key should not be cached yet"
        );

        storage
            .upload_image(&key, "image/png", b"fake-png-bytes".to_vec())
            .await
            .expect("upload_image should accept a validly-prefixed key");

        assert!(
            storage
                .check_cache(&key)
                .await
                .expect("check_cache after upload"),
            "key should be cached after upload"
        );

        let fetched = storage
            .get_image(&key)
            .await
            .expect("get_image should accept a validly-prefixed key");
        assert_eq!(fetched, b"fake-png-bytes".to_vec());
    }

    /// `delete` (#40) must actually remove an uploaded entry, and must be
    /// idempotent - deleting an already-absent key is still `Ok(())`, not an
    /// error - so a purge call never needs to check existence first.
    #[tokio::test]
    async fn delete_removes_entry_and_is_idempotent() {
        let dir = test_storage_dir();
        let storage_config =
            StorageConfig::new("http://cdn.test".to_string()).with_local_fs_config(&*dir);
        let storage = StorageService::new(storage_config).expect("build storage service");

        let key = format!(
            "{}.png",
            "a".repeat(64) // valid-shaped 64 lowercase hex-looking key
        );

        storage
            .upload_image(&key, "image/png", b"bytes".to_vec())
            .await
            .expect("upload_image");
        assert!(storage.check_cache(&key).await.unwrap());

        storage.delete(&key).await.expect("delete should succeed");
        assert!(!storage.check_cache(&key).await.unwrap());
        assert!(storage.get_image(&key).await.is_err());

        // Idempotent: deleting again (still absent) must not error.
        storage
            .delete(&key)
            .await
            .expect("deleting an already-absent key should still succeed");
    }

    /// A TTL'd entry (#40) must be reported absent by both `check_cache` and
    /// `get_image` once expired, and an entry with a future TTL must keep
    /// being served normally in the meantime.
    #[tokio::test]
    async fn ttl_expiry_is_honored_through_storage_service() {
        let dir = test_storage_dir();
        let storage_config =
            StorageConfig::new("http://cdn.test".to_string()).with_local_fs_config(&*dir);
        let storage = StorageService::new(storage_config).expect("build storage service");

        let expired_key = format!("{}.png", "b".repeat(64));
        let live_key = format!("{}.png", "c".repeat(64));

        storage
            .upload_image_with_ttl(
                &expired_key,
                "image/png",
                b"expired".to_vec(),
                Some(std::time::Duration::from_millis(1)),
            )
            .await
            .expect("upload_image_with_ttl (expired)");

        storage
            .upload_image_with_ttl(
                &live_key,
                "image/png",
                b"still alive".to_vec(),
                Some(std::time::Duration::from_secs(3600)),
            )
            .await
            .expect("upload_image_with_ttl (live)");

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;

        assert!(
            !storage.check_cache(&expired_key).await.unwrap(),
            "expired entry should be reported as a cache miss"
        );
        assert!(storage.get_image(&expired_key).await.is_err());

        assert!(
            storage.check_cache(&live_key).await.unwrap(),
            "not-yet-expired entry should still be a cache hit"
        );
        assert_eq!(
            storage.get_image(&live_key).await.unwrap(),
            b"still alive".to_vec()
        );
    }
}
