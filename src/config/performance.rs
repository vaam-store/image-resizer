use crate::modules::env::env::EnvConfig;
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
}

impl Default for PerformanceConfig {
    fn default() -> Self {
        Self {
            max_concurrent_downloads: 20,
            max_concurrent_processing: num_cpus::get(),
            http_timeout: Duration::from_secs(30),
            max_image_size: 50 * 1024 * 1024, // 50MB
            cpu_thread_pool_size: None,       // Use CPU count
            enable_http2: true,
            connection_pool_size: 50,
            keep_alive_timeout: Duration::from_secs(60),
        }
    }
}

impl PerformanceConfig {
    /// Create configuration optimized for high throughput
    pub fn high_throughput() -> Self {
        Self {
            max_concurrent_downloads: 50,
            max_concurrent_processing: num_cpus::get() * 2,
            http_timeout: Duration::from_secs(15),
            max_image_size: 100 * 1024 * 1024, // 100MB
            cpu_thread_pool_size: Some(num_cpus::get()),
            enable_http2: true,
            connection_pool_size: 100,
            keep_alive_timeout: Duration::from_secs(120),
        }
    }

    /// Create configuration optimized for low latency
    pub fn low_latency() -> Self {
        Self {
            max_concurrent_downloads: 10,
            max_concurrent_processing: num_cpus::get(),
            http_timeout: Duration::from_secs(10),
            max_image_size: 20 * 1024 * 1024, // 20MB
            cpu_thread_pool_size: Some(num_cpus::get()),
            enable_http2: true,
            connection_pool_size: 25,
            keep_alive_timeout: Duration::from_secs(30),
        }
    }

    /// Create configuration optimized for memory efficiency
    pub fn memory_efficient() -> Self {
        Self {
            max_concurrent_downloads: 5,
            max_concurrent_processing: num_cpus::get() / 2,
            http_timeout: Duration::from_secs(45),
            max_image_size: 10 * 1024 * 1024, // 10MB
            cpu_thread_pool_size: Some(num_cpus::get() / 2),
            enable_http2: false, // HTTP/1.1 uses less memory
            connection_pool_size: 10,
            keep_alive_timeout: Duration::from_secs(30),
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
    }

    /// Get optimal CPU thread pool size
    pub fn get_cpu_thread_pool_size(&self) -> usize {
        self.cpu_thread_pool_size.unwrap_or_else(num_cpus::get)
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
                .unwrap_or_else(num_cpus::get),
            http_timeout: Duration::from_secs(env_config.http_timeout_secs.unwrap_or_else(|| 30)),
            max_image_size: env_config.max_image_size_mb.unwrap_or_else(|| 50) * 1024 * 1024,
            cpu_thread_pool_size: env_config.cpu_thread_pool_size,
            enable_http2: env_config.enable_http2.unwrap_or(false),
            connection_pool_size: env_config.connection_pool_size.unwrap_or(50),
            keep_alive_timeout: Duration::from_secs(
                env_config.keep_alive_timeout_secs.unwrap_or(60),
            ),
        }
    }
}

/// Runtime performance metrics
#[derive(Debug, Default)]
pub struct PerformanceMetrics {
    pub active_downloads: std::sync::atomic::AtomicUsize,
    pub active_processing: std::sync::atomic::AtomicUsize,
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
        };

        let perf_config = PerformanceConfig::from(&env_config);

        assert_eq!(perf_config.max_concurrent_downloads, 20);
        assert_eq!(perf_config.max_concurrent_processing, num_cpus::get());
        assert_eq!(perf_config.http_timeout, Duration::from_secs(30));
        assert_eq!(perf_config.max_image_size, 50 * 1024 * 1024);
        assert_eq!(perf_config.cpu_thread_pool_size, None);
        assert_eq!(perf_config.enable_http2, true);
        assert_eq!(perf_config.connection_pool_size, 50);
        assert_eq!(perf_config.keep_alive_timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_performance_config_from_env_custom_values() {
        let env_config = EnvConfig {
            http_host: "0.0.0.0".to_string(),
            http_port: 3000,
            storage_type: None,
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
    }
}
