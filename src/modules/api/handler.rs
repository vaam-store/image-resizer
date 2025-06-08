use crate::modules::env::env::EnvConfig;
use crate::services::cache::handler::CacheServiceBuilder;
use crate::services::image::handler::ImageServiceBuilder;
use crate::services::resize::handler::{ResizeService, ResizeServiceBuilder};
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
        // Initialize cache service
        let cache_service = CacheServiceBuilder::default().build()?;

        // Image service
        let image_service = ImageServiceBuilder::default().build()?;

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

        // Initialize resize service
        let resize_service = ResizeServiceBuilder::default()
            .storage_service(storage_service)
            .cache_service(cache_service)
            .image_service(image_service)
            .build()?;

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
