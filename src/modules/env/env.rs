use envconfig::Envconfig;

#[derive(Envconfig, Clone)]
pub struct EnvConfig {
    #[envconfig(from = "HOST", default = "0.0.0.0")]
    pub http_host: String,

    #[envconfig(from = "PORT", default = "3000")]
    pub http_port: u16,

    #[envconfig(from = "STORAGE_TYPE")]
    pub storage_type: Option<String>,

    #[envconfig(from = "STORAGE_SUB_PATH", default = "")]
    pub sub_path: String,

    #[cfg(feature = "s3")]
    #[envconfig(from = "MINIO_ENDPOINT_URL", default = "http://localhost:9000")]
    pub minio_endpoint_url: String,

    #[cfg(feature = "s3")]
    #[envconfig(from = "MINIO_ACCESS_KEY_ID", default = "minioadmin")]
    pub minio_access_key_id: String,

    #[cfg(feature = "s3")]
    #[envconfig(from = "MINIO_SECRET_ACCESS_KEY", default = "minioadmin")]
    pub minio_secret_access_key: String,

    #[cfg(feature = "s3")]
    #[envconfig(from = "MINIO_BUCKET", default = "image-cache")]
    pub minio_bucket: String,

    #[cfg(feature = "s3")]
    #[envconfig(from = "MINIO_REGION", default = "us-east-1")]
    pub minio_region: String,

    #[cfg(feature = "local_fs")]
    #[envconfig(from = "LOCAL_FS_STORAGE_PATH", default = "./data/images")]
    pub local_fs_storage_path: String,

    #[envconfig(from = "CDN_BASE_URL", default = "http://localhost:9000/image-cache")]
    pub cdn_base_url: String,

    #[cfg(feature = "otel")]
    #[envconfig(from = "LOG_LEVEL", default = "debug")]
    pub log_level: String,

    #[cfg(feature = "otel")]
    #[envconfig(from = "OTLP_SPAN_ENDPOINT", default = "http://localhost:4317")]
    pub otlp_span_endpoint: String,

    #[cfg(feature = "otel")]
    #[envconfig(
        from = "OTLP_METRIC_ENDPOINT",
        default = "http://localhost:4318/v1/metrics"
    )]
    pub otlp_metric_endpoint: String,

    #[cfg(feature = "otel")]
    #[envconfig(from = "OTLP_SERVICE_NAME", default = "rust-app-example")]
    pub otlp_service_name: String,

    // Performance configuration
    #[envconfig(from = "MAX_CONCURRENT_DOWNLOADS")]
    pub max_concurrent_downloads: Option<usize>,

    #[envconfig(from = "MAX_CONCURRENT_PROCESSING")]
    pub max_concurrent_processing: Option<usize>,

    #[envconfig(from = "HTTP_TIMEOUT_SECS")]
    pub http_timeout_secs: Option<u64>,

    #[envconfig(from = "MAX_IMAGE_SIZE_MB")]
    pub max_image_size_mb: Option<u64>,

    #[envconfig(from = "CPU_THREAD_POOL_SIZE")]
    pub cpu_thread_pool_size: Option<usize>,

    #[envconfig(from = "ENABLE_HTTP2")]
    pub enable_http2: Option<bool>,

    #[envconfig(from = "CONNECTION_POOL_SIZE")]
    pub connection_pool_size: Option<usize>,

    #[envconfig(from = "KEEP_ALIVE_TIMEOUT_SECS")]
    pub keep_alive_timeout_secs: Option<u64>,

    #[envconfig(from = "PERFORMANCE_PROFILE")]
    pub performance_profile: Option<String>,

    // SSRF / source-fetch guard (#21)
    /// Maximum number of redirects the source fetch will follow, each hop
    /// re-validated (scheme, allowlist, resolved-address range). imgproxy
    /// equivalent: `IMGPROXY_MAX_REDIRECTS` / `MAX_REDIRECTS`.
    #[envconfig(from = "MAX_REDIRECTS")]
    pub max_redirects: Option<u8>,

    /// Comma-separated allowlist of source URL prefixes. When set, only
    /// source URLs matching at least one prefix are fetched. Unset (the
    /// default) allows any http(s) URL, subject to the private-range guard.
    /// imgproxy equivalent: `IMGPROXY_ALLOWED_SOURCES`.
    #[envconfig(from = "ALLOWED_SOURCES")]
    pub allowed_sources: Option<String>,

    /// Opt-in override to allow fetching from loopback addresses (default:
    /// blocked). imgproxy equivalent: `ALLOW_LOOPBACK_SOURCE_ADDRESSES`.
    #[envconfig(from = "ALLOW_LOOPBACK_SOURCE_ADDRESSES")]
    pub allow_loopback_source_addresses: Option<bool>,

    /// Opt-in override to allow fetching from link-local addresses
    /// (default: blocked). imgproxy equivalent:
    /// `ALLOW_LINK_LOCAL_SOURCE_ADDRESSES`.
    #[envconfig(from = "ALLOW_LINK_LOCAL_SOURCE_ADDRESSES")]
    pub allow_link_local_source_addresses: Option<bool>,

    // Resolution limits (#26)
    /// Maximum decoded *source* resolution in megapixels, checked against
    /// header dimensions before full decode. imgproxy equivalent:
    /// `IMGPROXY_MAX_SRC_RESOLUTION` (default 50).
    #[envconfig(from = "MAX_SRC_RESOLUTION_MP")]
    pub max_src_resolution_mp: Option<u64>,

    /// Maximum requested *output* width, enforced independently of
    /// whatever the generated OpenAPI layer does or does not validate.
    #[envconfig(from = "MAX_OUTPUT_WIDTH")]
    pub max_output_width: Option<u32>,

    /// Maximum requested *output* height, enforced independently of
    /// whatever the generated OpenAPI layer does or does not validate.
    #[envconfig(from = "MAX_OUTPUT_HEIGHT")]
    pub max_output_height: Option<u32>,
}
