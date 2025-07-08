use crate::config::performance::PerformanceConfig;
use crate::modules::env::env::EnvConfig;
use crate::services::cache::handler::CacheServiceBuilder;
use crate::services::resize::handler::ResizeService;
use crate::services::storage::handler::StorageService;
use anyhow::Result;
use derive_builder::Builder;
use gen_server::apis::ErrorHandler;

#[derive(Clone, Builder)]
pub struct ApiService {
    pub resize_service: ResizeService,
}

impl ApiService {
    pub fn create(config: EnvConfig) -> Result<Self> {
        // Create performance configuration from environment
        let performance_config = PerformanceConfig::from(&config);

        // Initialize cache service
        let cache_service = CacheServiceBuilder::default()
            .minio_sub_path(config.sub_path)
            .build()?;

        // Create storage config
        let mut storage_config =
            crate::services::storage::handler::StorageConfig::new(config.cdn_base_url);

        // Add storage type if specified
        if let Some(storage_type) = config.storage_type {
            storage_config = storage_config.with_storage_type(storage_type);
        }

        // Configure S3 storage
        #[cfg(feature = "s3")]
        {
            storage_config = storage_config.with_s3_config(
                config.minio_endpoint_url,
                config.minio_access_key_id,
                config.minio_secret_access_key,
                config.minio_bucket,
                config.minio_region,
            );
        }

        // Configure local FS storage
        #[cfg(feature = "local_fs")]
        {
            let path = std::path::PathBuf::from(config.local_fs_storage_path);

            storage_config = storage_config.with_local_fs_config(path);
        }

        // Create storage service
        let storage_service = StorageService::new(storage_config)?;

        // Initialize resize service with performance configuration
        let resize_service =
            ResizeService::with_config(storage_service, cache_service, performance_config)?;

        // Create API service
        let api_service = ApiServiceBuilder::default()
            .resize_service(resize_service)
            .build()?;

        Ok(api_service)
    }
}

impl ErrorHandler<()> for ApiService {}

impl AsRef<ApiService> for ApiService {
    fn as_ref(&self) -> &ApiService {
        self
    }
}
