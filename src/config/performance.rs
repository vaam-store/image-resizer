use crate::modules::env::env::EnvConfig;
// Cgroup-aware CPU count (#44): `num_cpus::get()` reads sched_getaffinity and
// is blind to a CFS quota, so a 400m-limited pod would size pools for the
// whole node and thrash on throttling.
use crate::modules::utils::cgroup::effective_cpu_count;
use std::time::Duration;

/// Performance configuration for the image resize service
#[derive(Debug, Clone)]
pub struct PerformanceConfig {
    /// Maximum concurrent downloads
    pub max_concurrent_downloads: usize,
    /// Maximum concurrent image processing tasks
    pub max_concurrent_processing: usize,
    /// HTTP client timeout
    pub http_timeout: Duration,
    /// Maximum image size in bytes (50MB default)
    pub max_image_size: u64,
    /// CPU thread pool size (defaults to CPU count)
    pub cpu_thread_pool_size: Option<usize>,
    /// Enable HTTP/2 for downloads
    pub enable_http2: bool,
    /// Connection pool size per host
    pub connection_pool_size: usize,
    /// Keep-alive timeout for connections
    pub keep_alive_timeout: Duration,
    /// Maximum number of redirects the source fetch will follow. Each hop
    /// is fully re-validated (scheme, allowlist, resolved address) - see
    /// `services::image::source_guard` (#21).
    pub max_redirects: u8,
    /// Optional allowlist of source URL prefixes (imgproxy's
    /// `ALLOWED_SOURCES` shape). `None`/empty means "no allowlist
    /// restriction" (still subject to the private-range guard). A host
    /// that matches an entry here is also exempted from the private-range
    /// (RFC1918/CGNAT/IPv6 ULA) block for that hop - see
    /// `source_guard::is_allowed_source` (#57). Loopback and link-local
    /// are unaffected; they have their own flags below.
    pub allowed_sources: Option<Vec<String>>,
    /// Opt-in override to allow fetching from loopback source addresses.
    /// Default `false` (blocked). See `ALLOW_LOOPBACK_SOURCE_ADDRESSES`.
    pub allow_loopback_source_addresses: bool,
    /// Opt-in override to allow fetching from link-local source addresses.
    /// Default `false` (blocked). See `ALLOW_LINK_LOCAL_SOURCE_ADDRESSES`.
    pub allow_link_local_source_addresses: bool,
    /// Maximum decoded *source* resolution in megapixels, checked against
    /// header dimensions before full decode (#26). imgproxy default: 50.
    pub max_src_resolution_mp: u64,
    /// Maximum requested *output* width in pixels (#26).
    pub max_output_width: u32,
    /// Maximum requested *output* height in pixels (#26).
    pub max_output_height: u32,
    /// Maximum number of frames read from an animated GIF/WebP source
    /// before the animated encode path (#49) gives up rather than keep
    /// decoding (a many-tiny-frames GIF is a real, cheap-to-craft
    /// decompression-bomb variant distinct from the large-single-frame one
    /// `max_src_resolution_mp` already guards against - each frame can be
    /// individually tiny in *resolution* while the frame *count* alone
    /// drives unbounded memory/CPU). Enforced while iterating
    /// (`ImageService::collect_frames_capped`), not after collecting every
    /// frame first.
    pub max_animation_frames: usize,
    /// Default watermark image URL (#52), used when a request sets `wm:`
    /// without its own `wmu:{base64url}`. `None` means a request must
    /// supply `wmu:` itself or `wm:` is an error - see
    /// `ImageService::process_image`. Fetched through the same SSRF guard
    /// as any other source URL.
    pub watermark_url: Option<String>,
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            max_concurrent_downloads: 20,
            max_concurrent_processing: effective_cpu_count(),
            http_timeout: Duration::from_secs(30),
            max_image_size: 50 * 1024 * 1024, // 50MB
            cpu_thread_pool_size: None,       // Use CPU count
            enable_http2: true,
            connection_pool_size: 50,
            keep_alive_timeout: Duration::from_secs(60),
            max_redirects: 5,
            allowed_sources: None,
            allow_loopback_source_addresses: false,
            allow_link_local_source_addresses: false,
            max_src_resolution_mp: 50,
            max_output_width: 4096,
            max_output_height: 4096,
            max_animation_frames: 512,
            watermark_url: None,
        }
    }
}

impl PerformanceConfig {
    /// Create configuration optimized for high throughput
    pub fn high_throughput() -> Self {
        Self {
            max_concurrent_downloads: 50,
            max_concurrent_processing: effective_cpu_count() * 2,
            http_timeout: Duration::from_secs(15),
            max_image_size: 100 * 1024 * 1024, // 100MB
            cpu_thread_pool_size: Some(effective_cpu_count()),
            enable_http2: true,
            connection_pool_size: 100,
            keep_alive_timeout: Duration::from_secs(120),
            max_redirects: 5,
            allowed_sources: None,
            allow_loopback_source_addresses: false,
            allow_link_local_source_addresses: false,
            max_src_resolution_mp: 50,
            max_output_width: 4096,
            max_output_height: 4096,
            max_animation_frames: 512,
            watermark_url: None,
        }
    }

    /// Create configuration optimized for low latency
    pub fn low_latency() -> Self {
        Self {
            max_concurrent_downloads: 10,
            max_concurrent_processing: effective_cpu_count(),
            http_timeout: Duration::from_secs(10),
            max_image_size: 20 * 1024 * 1024, // 20MB
            cpu_thread_pool_size: Some(effective_cpu_count()),
            enable_http2: true,
            connection_pool_size: 25,
            keep_alive_timeout: Duration::from_secs(30),
            max_redirects: 5,
            allowed_sources: None,
            allow_loopback_source_addresses: false,
            allow_link_local_source_addresses: false,
            max_src_resolution_mp: 50,
            max_output_width: 4096,
            max_output_height: 4096,
            max_animation_frames: 512,
            watermark_url: None,
        }
    }

    /// Create configuration optimized for memory efficiency
    pub fn memory_efficient() -> Self {
        Self {
            max_concurrent_downloads: 5,
            max_concurrent_processing: effective_cpu_count() / 2,
            http_timeout: Duration::from_secs(45),
            max_image_size: 10 * 1024 * 1024, // 10MB
            cpu_thread_pool_size: Some(effective_cpu_count() / 2),
            enable_http2: false, // HTTP/1.1 uses less memory
            connection_pool_size: 10,
            keep_alive_timeout: Duration::from_secs(30),
            max_redirects: 5,
            allowed_sources: None,
            allow_loopback_source_addresses: false,
            allow_link_local_source_addresses: false,
            max_src_resolution_mp: 25, // tighter than the 50MP default, in keeping with the smaller max_image_size
            max_output_width: 2048,
            max_output_height: 2048,
            max_animation_frames: 128, // tighter than the 512 default, same reasoning as the resolution/size caps above
            watermark_url: None,
        }
    }

    /// Create high throughput configuration with environment overrides
    fn high_throughput_from_env(env_config: &EnvConfig) -> Self {
        let mut config = Self::high_throughput();
        Self::apply_env_overrides(&mut config, env_config);
        config
    }

    /// Create low latency configuration with environment overrides
    fn low_latency_from_env(env_config: &EnvConfig) -> Self {
        let mut config = Self::low_latency();
        Self::apply_env_overrides(&mut config, env_config);
        config
    }

    /// Create memory efficient configuration with environment overrides
    fn memory_efficient_from_env(env_config: &EnvConfig) -> Self {
        let mut config = Self::memory_efficient();
        Self::apply_env_overrides(&mut config, env_config);
        config
    }

    /// Apply environment variable overrides to a configuration
    fn apply_env_overrides(config: &mut Self, env_config: &EnvConfig) {
        // Only override if the environment variable was explicitly set (not using defaults)
        // This allows preset profiles to work while still allowing fine-tuning

        if let Some(max_concurrent_downloads) = env_config.max_concurrent_downloads {
            config.max_concurrent_downloads = max_concurrent_downloads;
        }

        if let Some(max_processing) = env_config.max_concurrent_processing {
            config.max_concurrent_processing = max_processing;
        }

        if let Some(http_timeout_secs) = env_config.http_timeout_secs {
            config.http_timeout = Duration::from_secs(http_timeout_secs);
        }

        if let Some(max_image_size_mb) = env_config.max_image_size_mb {
            config.max_image_size = max_image_size_mb * 1024 * 1024;
        }

        if let Some(cpu_pool_size) = env_config.cpu_thread_pool_size {
            config.cpu_thread_pool_size = Some(cpu_pool_size);
        }

        if let Some(enable_http2) = env_config.enable_http2 {
            config.enable_http2 = enable_http2;
        }

        if let Some(connection_pool_size) = env_config.connection_pool_size {
            config.connection_pool_size = connection_pool_size;
        }

        if let Some(keep_alive_timeout) = env_config.keep_alive_timeout_secs {
            config.keep_alive_timeout = Duration::from_secs(keep_alive_timeout);
        }

        if let Some(max_redirects) = env_config.max_redirects {
            config.max_redirects = max_redirects;
        }

        if let Some(ref allowed_sources) = env_config.allowed_sources {
            config.allowed_sources = Self::parse_allowed_sources(allowed_sources);
        }

        if let Some(allow_loopback) = env_config.allow_loopback_source_addresses {
            config.allow_loopback_source_addresses = allow_loopback;
        }

        if let Some(allow_link_local) = env_config.allow_link_local_source_addresses {
            config.allow_link_local_source_addresses = allow_link_local;
        }

        if let Some(max_src_resolution_mp) = env_config.max_src_resolution_mp {
            config.max_src_resolution_mp = max_src_resolution_mp;
        }

        if let Some(max_output_width) = env_config.max_output_width {
            config.max_output_width = max_output_width;
        }

        if let Some(max_output_height) = env_config.max_output_height {
            config.max_output_height = max_output_height;
        }

        if let Some(max_animation_frames) = env_config.max_animation_frames {
            config.max_animation_frames = max_animation_frames;
        }

        if let Some(ref watermark_url) = env_config.watermark_url {
            config.watermark_url = Some(watermark_url.clone());
        }
    }

    /// Parses `ALLOWED_SOURCES`'s comma-separated-prefixes shape (imgproxy's
    /// `IMGPROXY_ALLOWED_SOURCES`) into a list of prefixes. Returns `None`
    /// for an empty/blank value, so "set but empty" behaves the same as
    /// "unset" (no allowlist restriction) rather than blocking everything.
    fn parse_allowed_sources(raw: &str) -> Option<Vec<String>> {
        let list: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();

        if list.is_empty() { None } else { Some(list) }
    }

    /// Get optimal CPU thread pool size
    pub fn get_cpu_thread_pool_size(&self) -> usize {
        self.cpu_thread_pool_size.unwrap_or_else(effective_cpu_count)
    }
}

impl From<&EnvConfig> for PerformanceConfig {
    fn from(env_config: &EnvConfig) -> Self {
        // Handle performance profile presets
        if let Some(ref profile) = env_config.performance_profile {
            match profile.to_lowercase().as_str() {
                "high_throughput" => return Self::high_throughput_from_env(env_config),
                "low_latency" => return Self::low_latency_from_env(env_config),
                "memory_efficient" => return Self::memory_efficient_from_env(env_config),
                _ => {} // Fall through to custom configuration
            }
        }

        Self {
            max_concurrent_downloads: env_config.max_concurrent_downloads.unwrap_or_else(|| 20),
            max_concurrent_processing: env_config
                .max_concurrent_processing
                .unwrap_or_else(effective_cpu_count),
            http_timeout: Duration::from_secs(env_config.http_timeout_secs.unwrap_or_else(|| 30)),
            max_image_size: env_config.max_image_size_mb.unwrap_or_else(|| 50) * 1024 * 1024,
            cpu_thread_pool_size: env_config.cpu_thread_pool_size,
            enable_http2: env_config.enable_http2.unwrap_or(false),
            connection_pool_size: env_config.connection_pool_size.unwrap_or(50),
            keep_alive_timeout: Duration::from_secs(
                env_config.keep_alive_timeout_secs.unwrap_or(60),
            ),
            max_redirects: env_config.max_redirects.unwrap_or(5),
            allowed_sources: env_config
                .allowed_sources
                .as_deref()
                .and_then(Self::parse_allowed_sources),
            allow_loopback_source_addresses: env_config
                .allow_loopback_source_addresses
                .unwrap_or(false),
            allow_link_local_source_addresses: env_config
                .allow_link_local_source_addresses
                .unwrap_or(false),
            max_src_resolution_mp: env_config.max_src_resolution_mp.unwrap_or(50),
            max_output_width: env_config.max_output_width.unwrap_or(4096),
            max_output_height: env_config.max_output_height.unwrap_or(4096),
            max_animation_frames: env_config.max_animation_frames.unwrap_or(512),
            watermark_url: env_config.watermark_url.clone(),
        }
    }
}

/// Runtime performance metrics
#[derive(Debug, Default)]
pub struct PerformanceMetrics {
    pub total_requests: std::sync::atomic::AtomicU64,
    pub cache_hits: std::sync::atomic::AtomicU64,
    pub cache_misses: std::sync::atomic::AtomicU64,
    pub avg_download_time_ms: std::sync::atomic::AtomicU64,
    pub avg_processing_time_ms: std::sync::atomic::AtomicU64,
    pub avg_upload_time_ms: std::sync::atomic::AtomicU64,
}

impl PerformanceMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment_requests(&self) {
        self.total_requests
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn increment_cache_hits(&self) {
        self.cache_hits
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn increment_cache_misses(&self) {
        self.cache_misses
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn get_cache_hit_ratio(&self) -> f64 {
        let hits = self.cache_hits.load(std::sync::atomic::Ordering::Relaxed);
        let misses = self.cache_misses.load(std::sync::atomic::Ordering::Relaxed);
        let total = hits + misses;

        if total == 0 {
            0.0
        } else {
            hits as f64 / total as f64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::env::env::EnvConfig;
    use std::time::Duration;

    #[test]
    fn test_performance_config_from_env_defaults() {
        // Create EnvConfig with default values
        let env_config = EnvConfig {
            http_host: "0.0.0.0".to_string(),
            http_port: 3000,
            storage_type: None,
            sub_path: "".to_string(),
            #[cfg(feature = "s3")]
            minio_endpoint_url: "http://localhost:9000".to_string(),
            #[cfg(feature = "s3")]
            minio_access_key_id: "minioadmin".to_string(),
            #[cfg(feature = "s3")]
            minio_secret_access_key: "minioadmin".to_string(),
            #[cfg(feature = "s3")]
            minio_bucket: "image-cache".to_string(),
            #[cfg(feature = "s3")]
            minio_region: "us-east-1".to_string(),
            #[cfg(feature = "local_fs")]
            local_fs_storage_path: "./data/images".to_string(),
            cdn_base_url: "http://localhost:9000/image-cache".to_string(),
            #[cfg(feature = "otel")]
            log_level: "debug".to_string(),
            #[cfg(feature = "otel")]
            otlp_span_endpoint: "http://localhost:4317".to_string(),
            #[cfg(feature = "otel")]
            otlp_metric_endpoint: "http://localhost:4318/v1/metrics".to_string(),
            #[cfg(feature = "otel")]
            otlp_service_name: "rust-app-example".to_string(),
            // Performance settings
            max_concurrent_downloads: Some(20),
            max_concurrent_processing: None,
            http_timeout_secs: Some(30),
            max_image_size_mb: Some(50),
            cpu_thread_pool_size: None,
            enable_http2: Some(true),
            connection_pool_size: Some(50),
            keep_alive_timeout_secs: Some(60),
            performance_profile: None,
            max_redirects: None,
            allowed_sources: None,
            allow_loopback_source_addresses: None,
            allow_link_local_source_addresses: None,
            max_src_resolution_mp: None,
            max_output_width: None,
            max_output_height: None,
            max_animation_frames: None,
            signing_key: None,
            signing_salt: None,
            allow_unsigned_requests: None,
            watermark_url: None,
            presets: None,
            allowed_processing_options: None,
        };

        let perf_config = PerformanceConfig::from(&env_config);

        assert_eq!(perf_config.max_concurrent_downloads, 20);
        assert_eq!(perf_config.max_concurrent_processing, effective_cpu_count());
        assert_eq!(perf_config.http_timeout, Duration::from_secs(30));
        assert_eq!(perf_config.max_image_size, 50 * 1024 * 1024);
        assert_eq!(perf_config.cpu_thread_pool_size, None);
        assert_eq!(perf_config.enable_http2, true);
        assert_eq!(perf_config.connection_pool_size, 50);
        assert_eq!(perf_config.keep_alive_timeout, Duration::from_secs(60));
        assert_eq!(perf_config.max_redirects, 5);
        assert_eq!(perf_config.allowed_sources, None);
        assert_eq!(perf_config.allow_loopback_source_addresses, false);
        assert_eq!(perf_config.allow_link_local_source_addresses, false);
        assert_eq!(perf_config.max_src_resolution_mp, 50);
        assert_eq!(perf_config.max_output_width, 4096);
        assert_eq!(perf_config.max_output_height, 4096);
        assert_eq!(perf_config.max_animation_frames, 512);
        assert_eq!(perf_config.watermark_url, None);
    }

    #[test]
    fn test_performance_config_from_env_custom_values() {
        let env_config = EnvConfig {
            http_host: "0.0.0.0".to_string(),
            http_port: 3000,
            storage_type: None,
            sub_path: "".to_string(),
            #[cfg(feature = "s3")]
            minio_endpoint_url: "http://localhost:9000".to_string(),
            #[cfg(feature = "s3")]
            minio_access_key_id: "minioadmin".to_string(),
            #[cfg(feature = "s3")]
            minio_secret_access_key: "minioadmin".to_string(),
            #[cfg(feature = "s3")]
            minio_bucket: "image-cache".to_string(),
            #[cfg(feature = "s3")]
            minio_region: "us-east-1".to_string(),
            #[cfg(feature = "local_fs")]
            local_fs_storage_path: "./data/images".to_string(),
            cdn_base_url: "http://localhost:9000/image-cache".to_string(),
            #[cfg(feature = "otel")]
            log_level: "debug".to_string(),
            #[cfg(feature = "otel")]
            otlp_span_endpoint: "http://localhost:4317".to_string(),
            #[cfg(feature = "otel")]
            otlp_metric_endpoint: "http://localhost:4318/v1/metrics".to_string(),
            #[cfg(feature = "otel")]
            otlp_service_name: "rust-app-example".to_string(),
            // Custom performance settings
            max_concurrent_downloads: Some(100),
            max_concurrent_processing: Some(8),
            http_timeout_secs: Some(15),
            max_image_size_mb: Some(100),
            cpu_thread_pool_size: Some(4),
            enable_http2: Some(false),
            connection_pool_size: Some(25),
            keep_alive_timeout_secs: Some(120),
            performance_profile: None,
            max_redirects: Some(3),
            allowed_sources: Some(
                "https://trusted.example.com/, https://cdn.example.net/".to_string(),
            ),
            allow_loopback_source_addresses: Some(true),
            allow_link_local_source_addresses: Some(true),
            max_src_resolution_mp: Some(80),
            max_output_width: Some(2048),
            max_output_height: Some(1024),
            max_animation_frames: Some(256),
            signing_key: None,
            signing_salt: None,
            allow_unsigned_requests: None,
            watermark_url: Some("https://cdn.example.com/logo.png".to_string()),
            presets: None,
            allowed_processing_options: None,
        };

        let perf_config = PerformanceConfig::from(&env_config);

        assert_eq!(perf_config.max_concurrent_downloads, 100);
        assert_eq!(perf_config.max_concurrent_processing, 8);
        assert_eq!(perf_config.http_timeout, Duration::from_secs(15));
        assert_eq!(perf_config.max_image_size, 100 * 1024 * 1024);
        assert_eq!(perf_config.cpu_thread_pool_size, Some(4));
        assert_eq!(perf_config.enable_http2, false);
        assert_eq!(perf_config.connection_pool_size, 25);
        assert_eq!(perf_config.keep_alive_timeout, Duration::from_secs(120));
        assert_eq!(perf_config.max_redirects, 3);
        assert_eq!(
            perf_config.allowed_sources,
            Some(vec![
                "https://trusted.example.com/".to_string(),
                "https://cdn.example.net/".to_string(),
            ])
        );
        assert_eq!(perf_config.allow_loopback_source_addresses, true);
        assert_eq!(perf_config.allow_link_local_source_addresses, true);
        assert_eq!(perf_config.max_src_resolution_mp, 80);
        assert_eq!(perf_config.max_output_width, 2048);
        assert_eq!(perf_config.max_output_height, 1024);
        assert_eq!(perf_config.max_animation_frames, 256);
        assert_eq!(
            perf_config.watermark_url,
            Some("https://cdn.example.com/logo.png".to_string())
        );
    }

    #[test]
    fn test_allowed_sources_blank_value_is_none() {
        let raw = "  , ,";
        assert_eq!(PerformanceConfig::parse_allowed_sources(raw), None);
    }
}
