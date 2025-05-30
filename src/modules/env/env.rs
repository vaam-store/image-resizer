use envconfig::Envconfig;

#[derive(Envconfig, Clone)]
pub struct EnvConfig {
    #[envconfig(from = "HOST", default = "0.0.0.0")]
    pub http_host: String,

    #[envconfig(from = "PORT", default = "3000")]
    pub http_port: u16,

    #[envconfig(from = "LOG_LEVEL", default = "debug")]
    pub log_level: String,

    #[envconfig(
        from = "RUST_LOG",
        default = "image_resizer=debug,tower_http=info,reqwest=info,aws_sdk_s3=info,hyper=info"
    )]
    pub rust_log: String,

    #[envconfig(from = "STORAGE_TYPE", default = "LOCAL_FS")]
    pub storage_type: String,

    #[envconfig(from = "MINIO_ENDPOINT_URL", default = "http://localhost:9000")]
    pub minio_endpoint_url: String,

    #[envconfig(from = "MINIO_ACCESS_KEY_ID", default = "minioadmin")]
    pub minio_access_key_id: String,

    #[envconfig(from = "MINIO_SECRET_ACCESS_KEY", default = "minioadmin")]
    pub minio_secret_access_key: String,

    #[envconfig(from = "MINIO_BUCKET", default = "image-cache")]
    pub minio_bucket: String,

    #[envconfig(from = "MINIO_REGION", default = "us-east-1")]
    pub minio_region: String,

    #[envconfig(from = "LOCAL_FS_STORAGE_PATH", default = "./data/images")]
    pub local_fs_storage_path: String,

    #[envconfig(from = "CDN_BASE_URL", default = "http://localhost:9000/image-cache")]
    pub cdn_base_url: String,

    #[envconfig(from = "MAX_IMAGE_WIDTH", default = "2000")]
    pub max_image_width: u32,

    #[envconfig(from = "MAX_IMAGE_HEIGHT", default = "2000")]
    pub max_image_height: u32,

    #[envconfig(from = "PROMETHEUS_METRICS_PORT", default = "9001")]
    pub prometheus_metrics_port: u16,

    #[envconfig(from = "OTLP_SPAN_ENDPOINT", default = "http://localhost:4317")]
    pub otlp_span_endpoint: String,

    #[envconfig(
        from = "OTLP_METRIC_ENDPOINT",
        default = "http://localhost:4318/v1/metrics"
    )]
    pub otlp_metric_endpoint: String,

    #[envconfig(from = "OTLP_SERVICE_NAME", default = "rust-app-example")]
    pub otlp_service_name: String,

    #[envconfig(from = "OTLP_VERSION", default = "0.1.0")]
    pub otlp_version: String,
}
