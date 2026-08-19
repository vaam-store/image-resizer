use crate::config::performance::PerformanceConfig;
use crate::modules::env::env::EnvConfig;
use crate::modules::utils::err::AppError;
use crate::services::cache::handler::CacheServiceBuilder;
use crate::services::resize::handler::ResizeService;
use crate::services::storage::handler::StorageService;
use anyhow::Result;
use async_trait::async_trait;
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum_extra::extract::CookieJar;
use derive_builder::Builder;
use gen_server::apis::ErrorHandler;
use headers::Host;

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
            .minio_sub_path(config.sub_path.clone())
            .build()?;

        // Create storage config. `key_prefix` must match `sub_path` above -
        // `CacheService::generate_key` prepends it to every key it hands
        // out, and `StorageService` rejects any key that doesn't start with
        // exactly this prefix (#23). Leaving this unset (the `""` default)
        // while `STORAGE_SUB_PATH` is non-empty means every legitimately
        // generated key gets rejected as invalid - the service breaks
        // entirely, silently, only on deployments that set a non-default
        // sub-path.
        let mut storage_config =
            crate::services::storage::handler::StorageConfig::new(config.cdn_base_url)
                .with_key_prefix(config.sub_path);

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

/// Turns an `AppError` returned from an `Images` handler (see
/// `src/modules/api/resize.rs`) into a real HTTP response with the correct
/// status code, bypassing the generated `DownloadResponse`/`ResizeResponse`
/// enums entirely. This is the generated router's own extension point
/// (`packages/gen-server/src/server/mod.rs` calls `handle_error` whenever
/// the trait method returns `Err`), so no OpenAPI regeneration is needed to
/// add error status codes (#41, #25).
#[async_trait]
impl ErrorHandler<AppError> for ApiService {
    async fn handle_error(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        error: AppError,
    ) -> Result<Response, StatusCode> {
        Ok(error.into_response())
    }
}

impl AsRef<ApiService> for ApiService {
    fn as_ref(&self) -> &ApiService {
        self
    }
}
