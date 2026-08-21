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
    ///
    /// A match here is also authoritative for the private-IP-range block
    /// (RFC1918/CGNAT/IPv6 ULA) - #57: an operator explicitly naming a
    /// source (a Kubernetes Service ClusterIP, an internal MinIO, a
    /// private CDN shield) makes that specific host reachable even though
    /// it resolves to a private address, without weakening the guard for
    /// anything else. Loopback and link-local are unaffected by this -
    /// they keep their own separate opt-in flags below, so the cloud
    /// metadata endpoint stays hard to reach even from an allowlisted
    /// origin's redirect. Re-checked on every redirect hop, not just the
    /// original URL. imgproxy equivalent: `IMGPROXY_ALLOWED_SOURCES`.
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

    /// Maximum number of frames read from an animated GIF/WebP source
    /// before the animated encode path (#49) refuses the request - guards
    /// against a many-tiny-frames animation bomb that `MAX_SRC_RESOLUTION_MP`
    /// doesn't catch (each frame can be individually small).
    #[envconfig(from = "MAX_ANIMATION_FRAMES")]
    pub max_animation_frames: Option<usize>,

    // Signed URLs (#27)
    /// Hex-encoded HMAC-SHA256 key used to verify signed URLs. Required
    /// unless `ALLOW_UNSIGNED_REQUESTS=true` - signing is the default, not
    /// opt-in (`src/modules/signing`). imgproxy equivalent: `IMGPROXY_KEY`.
    #[envconfig(from = "SIGNING_KEY")]
    pub signing_key: Option<String>,

    /// Hex-encoded salt mixed into every signed URL's HMAC input. Required
    /// unless `ALLOW_UNSIGNED_REQUESTS=true`. imgproxy equivalent:
    /// `IMGPROXY_SALT`.
    #[envconfig(from = "SIGNING_SALT")]
    pub signing_salt: Option<String>,

    /// Opt-in escape hatch for local development: when `true`, a request
    /// whose signature segment is the literal `unsigned` bypasses signature
    /// verification entirely. Default `false` - signing itself is always
    /// the default, this only ever widens the `unsigned` escape path.
    #[envconfig(from = "ALLOW_UNSIGNED_REQUESTS")]
    pub allow_unsigned_requests: Option<bool>,

    // Watermarking (#52)
    /// Default watermark image URL, used when a request sets `wm:` without
    /// its own `wmu:{base64url}`. Fetched through the same SSRF guard
    /// (`services::image::source_guard`, #21/#57) as any other source URL -
    /// see `ImageService::process_image`. imgproxy equivalent:
    /// `IMGPROXY_WATERMARK_URL`.
    #[envconfig(from = "WATERMARK_URL")]
    pub watermark_url: Option<String>,

    // Presets and the processing-option allowlist (#52)
    /// Preset definitions: comma-separated `{name}={options}` entries,
    /// `{options}` itself `/`-separated processing-option segments - e.g.
    /// `thumbnail=rs:fill:300:300/q:80,default=el:1`. See
    /// `modules::url::presets::PresetRegistry::parse`. imgproxy
    /// equivalent: `IMGPROXY_PRESETS`.
    #[envconfig(from = "PRESETS")]
    pub presets: Option<String>,

    /// Comma-separated allowlist of processing-option short codes (e.g.
    /// `rs,q,pr`) permitted directly in a request URL. Unset/blank means
    /// unrestricted. Does not apply to options used *inside* a preset's own
    /// definition - see `modules::url::presets::AllowedOptions`. imgproxy
    /// equivalent: `IMGPROXY_ALLOWED_PROCESSING_OPTIONS`.
    #[envconfig(from = "ALLOWED_PROCESSING_OPTIONS")]
    pub allowed_processing_options: Option<String>,

    // /metrics authentication (#77, #27 leftover). Gated behind `otel`
    // like the other Observability variables above - `/metrics` itself is
    // only ever mounted (`src/modules/router/router.rs`) and only ever
    // has anything to serve (`prometheus::gather`) when the binary is
    // built with `--features otel`, so a build without it has no
    // endpoint to protect and no reason to demand this at startup.
    /// Bearer token required on every `/metrics` request. Required unless
    /// `ALLOW_UNAUTHENTICATED_METRICS=true` - matching how `SIGNING_KEY`/
    /// `SIGNING_SALT` are required unless `ALLOW_UNSIGNED_REQUESTS=true`
    /// (`src/modules/signing/config.rs`). See
    /// `src/modules/metrics_auth/config.rs`.
    #[cfg(feature = "otel")]
    #[envconfig(from = "METRICS_AUTH_TOKEN")]
    pub metrics_auth_token: Option<String>,

    /// Opt-in escape hatch: when `true`, `/metrics` is served without
    /// requiring a bearer token. Default `false` - requiring a token is
    /// the default, this only ever widens the unauthenticated-access
    /// escape path (never weakens verification of a real token).
    #[cfg(feature = "otel")]
    #[envconfig(from = "ALLOW_UNAUTHENTICATED_METRICS")]
    pub allow_unauthenticated_metrics: Option<bool>,
}
