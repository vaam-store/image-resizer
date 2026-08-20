use crate::config::performance::PerformanceConfig;
use crate::modules::env::env::EnvConfig;
use crate::modules::signing::SigningConfig;
use crate::modules::url::presets::{AllowedOptions, PresetRegistry};
use crate::services::cache::handler::CacheServiceBuilder;
use crate::services::resize::handler::ResizeService;
use crate::services::storage::handler::StorageService;
use anyhow::{Context, Result};
use derive_builder::Builder;

/// Shared application state (#53: replaces the generated `gen_server`
/// router's `ApiService` - same role, but there's no `ErrorHandler` trait
/// to implement any more: handlers in `src/modules/api/{resize,download}.rs`
/// build `AppError` responses directly).
#[derive(Clone, Builder)]
pub struct ApiService {
    pub resize_service: ResizeService,
    pub signing: SigningConfig,
    /// Preset definitions (#52) - see `PresetRegistry::parse` for the
    /// `PRESETS` config format. `default: PresetRegistry::empty()` (no
    /// presets configured) when unset via `ApiServiceBuilder`, so existing
    /// tests that build an `ApiService` without ever mentioning presets are
    /// unaffected.
    #[builder(default = "PresetRegistry::empty()")]
    pub presets: PresetRegistry,
    /// Processing-option allowlist (#52) - see `AllowedOptions::parse` for
    /// the `ALLOWED_PROCESSING_OPTIONS` config format. Defaults to
    /// unrestricted.
    #[builder(default = "AllowedOptions::unrestricted()")]
    pub allowed_options: AllowedOptions,
    /// `/metrics` authentication (#77). Gated behind `otel` like the
    /// endpoint it protects - see `crate::modules::metrics_auth`.
    #[cfg(feature = "otel")]
    #[builder(default = "crate::modules::metrics_auth::MetricsAuthConfig::default()")]
    pub metrics_auth: crate::modules::metrics_auth::MetricsAuthConfig,
}

impl ApiService {
    pub fn create(config: EnvConfig) -> Result<Self> {
        // Read before anything below partially moves individual fields out
        // of `config` - `SigningConfig::from_env` only needs a shared
        // borrow, so it must run first (fails closed at startup per #27 if
        // signing isn't configured and wasn't explicitly opted out of).
        let signing = SigningConfig::from_env(&config)?;

        // `/metrics` authentication (#77) - fails closed at startup exactly
        // like `signing` above, and for the same reason: a deployment with
        // neither a real token nor an explicit opt-out could never tell
        // "forgot to configure this" apart from "meant to leave it open".
        // Gated behind `otel` since that's the only build `/metrics` is
        // ever mounted in (`src/modules/router/router.rs`).
        #[cfg(feature = "otel")]
        let metrics_auth = crate::modules::metrics_auth::MetricsAuthConfig::from_env(&config)?;

        // Presets (#52) - fails closed at startup (mirrors `signing` above)
        // rather than deferring a broken `PRESETS` value to the first
        // request that happens to use one.
        let presets = PresetRegistry::parse(config.presets.as_deref().unwrap_or(""))
            .map_err(|e| anyhow::anyhow!("invalid PRESETS configuration: {e}"))
            .context("failed to build preset registry")?;
        let allowed_options =
            AllowedOptions::parse(config.allowed_processing_options.as_deref().unwrap_or(""));

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
        let mut api_service_builder = ApiServiceBuilder::default();
        api_service_builder
            .resize_service(resize_service)
            .signing(signing)
            .presets(presets)
            .allowed_options(allowed_options);

        #[cfg(feature = "otel")]
        api_service_builder.metrics_auth(metrics_auth);

        let api_service = api_service_builder.build()?;

        Ok(api_service)
    }
}

impl AsRef<ApiService> for ApiService {
    fn as_ref(&self) -> &ApiService {
        self
    }
}
