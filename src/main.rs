use anyhow::Context;
use aws_sdk_s3 as s3;
use aws_sdk_s3::primitives::ByteStream;
use axum::{
    Router,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Redirect},
    routing::get,
};
use image::{ImageFormat, imageops::FilterType};
use serde::Deserialize;
use std::{env, io::Cursor, net::SocketAddr, path::PathBuf, sync::Arc};
use validator::Validate;

// Telemetry
use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};
use metrics_exporter_prometheus::PrometheusBuilder;
use opentelemetry::{KeyValue, global};
use opentelemetry_sdk::{Resource, propagation::TraceContextPropagator, trace::TracerProvider};
use tracing::{Level, debug, error, info, instrument, span};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt}; // For stdout exporter in dev

// For hashing the cache key
use hex;
use sha2::{Digest, Sha256};

// For async traits
use async_trait::async_trait;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::operation::head_object::HeadObjectError;
use opentelemetry_otlp::{new_exporter, LogExporter, SpanExporter, SpanExporterBuilder, WithExportConfig};
// For static file serving if using LocalFS
use tower_http::services::ServeDir;

// --- 1. Query Parameters Struct with Validation ---
#[derive(Debug, Deserialize, Validate)]
struct ResizeParams {
    #[validate(url)]
    url: String,
    // Max values for width/height will be dynamically set by AppState limits
    #[validate(range(min = 1))]
    width: u32,
    #[validate(range(min = 1))]
    height: u32,
    #[validate(custom(function = "validate_format"))]
    format: String, // e.g., "jpeg", "png", "webp"
}

// Custom validator for image format
fn validate_format(format: &str) -> Result<(), validator::ValidationError> {
    match format.to_lowercase().as_str() {
        "jpeg" | "jpg" | "png" | "webp" | "gif" | "bmp" | "tiff" => Ok(()),
        _ => Err(validator::ValidationError::new("unsupported_format")),
    }
}

// --- 2. Storage Backend Trait (Delegation Pattern) ---
#[async_trait]
trait StorageBackend: Send + Sync + 'static {
    /// Uploads image data to the storage backend with a given key and content type.
    async fn upload_image(
        &self,
        key: &str,
        content_type: &str,
        data: Vec<u8>,
    ) -> Result<(), AppError>;

    /// Checks if an object with the given key exists in the storage backend.
    async fn check_cache(&self, key: &str) -> Result<bool, AppError>;
}

// --- 2.1. MinIO Storage Implementation ---
struct MinIOStorage {
    client: s3::Client,
    bucket: String,
}

#[async_trait]
impl StorageBackend for MinIOStorage {
    async fn upload_image(
        &self,
        key: &str,
        content_type: &str,
        data: Vec<u8>,
    ) -> Result<(), AppError> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(data))
            .content_type(content_type)
            .send()
            .await
            .map_err(|e| AppError::S3(Box::new(e) as Box<dyn std::error::Error + Send + Sync>))
            .context("Failed to upload image to MinIO")?;
        Ok(())
    }

    async fn check_cache(&self, key: &str) -> Result<bool, AppError> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(_) => Ok(true),
            Err(sdk_err) => match sdk_err.into_service_error() {
                HeadObjectError::NotFound(_) => Ok(false),
                err => Err(AppError::S3(Box::new(err))),
            },
        }
    }
}

// --- 2.2. Local File System Storage Implementation ---
struct LocalFSStorage {
    base_path: PathBuf,
}

#[async_trait]
impl StorageBackend for LocalFSStorage {
    async fn upload_image(
        &self,
        key: &str,
        _content_type: &str,
        data: Vec<u8>,
    ) -> Result<(), AppError> {
        let file_path = self.base_path.join(key);
        // Ensure directory exists
        if let Some(parent) = file_path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("Failed to create a local storage directory")?;
        }
        tokio::fs::write(&file_path, data)
            .await
            .context("Failed to write image to a local file system")?;
        Ok(())
    }

    async fn check_cache(&self, key: &str) -> Result<bool, AppError> {
        let file_path = self.base_path.join(key);
        Ok(tokio::fs::metadata(&file_path).await.is_ok())
    }
}

// --- 3. Application State ---
#[derive(Clone)]
struct AppState {
    storage: Arc<dyn StorageBackend>, // Holds the chosen storage implementation
    cdn_base_url: String,
    max_image_width: u32,
    max_image_height: u32,
}

// --- 4. Custom Error Handling ---
// This enum defines all possible errors and how they map to HTTP responses.
#[derive(Debug)]
enum AppError {
    Validation(validator::ValidationErrors),
    Reqwest(reqwest::Error),
    Image(image::ImageError),
    S3(Box<dyn std::error::Error + Send + Sync>),
    Io(std::io::Error),
    Config(String),
    Anyhow(anyhow::Error),
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppError::Validation(e) => write!(f, "Validation error: {}", e),
            AppError::Reqwest(e) => write!(f, "HTTP request error: {}", e),
            AppError::Image(e) => write!(f, "Image processing error: {}", e),
            AppError::S3(e) => write!(f, "S3 storage error: {}", e),
            AppError::Io(e) => write!(f, "I/O error: {}", e),
            AppError::Config(e) => write!(f, "Configuration error: {}", e),
            AppError::Anyhow(e) => write!(f, "Internal error: {}", e),
        }
    }
}

impl std::error::Error for AppError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AppError::Validation(e) => Some(e),
            AppError::Reqwest(e) => Some(e),
            AppError::Image(e) => Some(e),
            AppError::S3(e) => Some(e.as_ref()),
            AppError::Io(e) => Some(e),
            AppError::Config(_) => None,
            AppError::Anyhow(e) => Some(e.as_ref()),
        }
    }
}

// Implement conversion from external error types to AppError
impl From<validator::ValidationErrors> for AppError {
    fn from(err: validator::ValidationErrors) -> Self {
        AppError::Validation(err)
    }
}
impl From<reqwest::Error> for AppError {
    fn from(err: reqwest::Error) -> Self {
        AppError::Reqwest(err)
    }
}
impl From<image::ImageError> for AppError {
    fn from(err: image::ImageError) -> Self {
        AppError::Image(err)
    }
}
impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::Io(err)
    }
}
impl From<anyhow::Error> for AppError {
    fn from(err: anyhow::Error) -> Self {
        AppError::Anyhow(err)
    }
}

// Implement IntoResponse for AppError to convert it to HTTP responses
impl IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        let (status, error_message) = match self {
            AppError::Validation(err) => (
                StatusCode::BAD_REQUEST,
                format!("Input validation failed: {:?}", err),
            ),
            AppError::Reqwest(err) => (
                StatusCode::BAD_GATEWAY,
                format!("Failed to download image: {}", err),
            ),
            AppError::Image(err) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("Image processing error: {}", err),
            ),
            AppError::S3(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("S3 storage error: {}", err),
            ),
            AppError::Io(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("IO error: {}", err),
            ),
            AppError::Config(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Configuration error: {}", msg),
            ),
            AppError::Anyhow(err) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("An internal error occurred: {}", err),
            ),
        };
        error!("Error: {}", error_message); // Log the error
        (status, error_message).into_response()
    }
}

// --- 5. Main Function and Server Setup ---
#[tokio::main]
async fn main() -> Result<(), AppError> {
    dotenvy::dotenv().ok(); // Load .env file if it exists

    // Initialize Telemetry
    init_telemetry();
    info!("Starting image resizer server...");

    // Register Metrics
    register_metrics();

    // Load Configuration
    let storage_type = env::var("STORAGE_TYPE")
        .unwrap_or_else(|_| "MINIO".to_string())
        .to_uppercase();

    let cdn_base_url =
        env::var("CDN_BASE_URL").map_err(|_| AppError::Config("CDN_BASE_URL not set".into()))?;
    let max_image_width: u32 = env::var("MAX_IMAGE_WIDTH")
        .unwrap_or_else(|_| "2000".to_string())
        .parse()
        .map_err(|_| AppError::Config("MAX_IMAGE_WIDTH must be a number".into()))?;
    let max_image_height: u32 = env::var("MAX_IMAGE_HEIGHT")
        .unwrap_or_else(|_| "2000".to_string())
        .parse()
        .map_err(|_| AppError::Config("MAX_IMAGE_HEIGHT must be a number".into()))?;

    // --- Initialize Storage Backend based on STORAGE_TYPE ---
    let storage: Arc<dyn StorageBackend> = match storage_type.as_str() {
        "MINIO" => {
            let minio_endpoint_url = env::var("MINIO_ENDPOINT_URL").map_err(|_| {
                AppError::Config("MINIO_ENDPOINT_URL not set for MINIO storage".into())
            })?;
            let minio_access_key = env::var("MINIO_ACCESS_KEY_ID").map_err(|_| {
                AppError::Config("MINIO_ACCESS_KEY_ID not set for MINIO storage".into())
            })?;
            let minio_secret_key = env::var("MINIO_SECRET_ACCESS_KEY").map_err(|_| {
                AppError::Config("MINIO_SECRET_ACCESS_KEY not set for MINIO storage".into())
            })?;
            let minio_bucket = env::var("MINIO_BUCKET")
                .map_err(|_| AppError::Config("MINIO_BUCKET not set for MINIO storage".into()))?;
            let minio_region = env::var("MINIO_REGION").unwrap_or_else(|_| "us-east-1".to_string());

            let shared_config =
                aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;
            let s3_config = s3::config::Builder::from(&shared_config)
                .endpoint_url(minio_endpoint_url)
                .credentials_provider(s3::config::Credentials::new(
                    minio_access_key,
                    minio_secret_key,
                    None,     // session_token
                    None,     // expiry
                    "Static", // provider_name
                ))
                .region(s3::config::Region::new(minio_region))
                .force_path_style(true) // Crucial for MinIO compatibility
                .build();

            let s3_client = s3::Client::from_conf(s3_config);
            info!("Using MinIO storage backend, bucket: {}", minio_bucket);
            Arc::new(MinIOStorage {
                client: s3_client,
                bucket: minio_bucket,
            })
        }
        "LOCAL_FS" => {
            let local_fs_storage_path = env::var("LOCAL_FS_STORAGE_PATH").map_err(|_| {
                AppError::Config("LOCAL_FS_STORAGE_PATH not set for LOCAL_FS storage".into())
            })?;
            let path = PathBuf::from(local_fs_storage_path);
            info!(
                "Using Local File System storage backend at path: {:?}",
                path
            );
            Arc::new(LocalFSStorage { base_path: path })
        }
        _ => {
            return Err(AppError::Config(format!(
                "Invalid STORAGE_TYPE: {}",
                storage_type
            )));
        }
    };

    let app_state = AppState {
        storage,
        cdn_base_url,
        max_image_width,
        max_image_height,
    };

    // --- Axum Router Setup ---
    let mut app = Router::new()
        .route("/resize", get(resize_handler))
        .route("/metrics", get(metrics_handler));

    // If using LocalFS, also serve static files from the storage path
    if storage_type.as_str() == "LOCAL_FS" {
        let local_fs_storage_path = env::var("LOCAL_FS_STORAGE_PATH")
            .map_err(|_| AppError::Config("LOCAL_FS_STORAGE_PATH not set for LOCAL_FS".into()))?;
        let path = PathBuf::from(local_fs_storage_path);
        app = app.nest_service("/static", ServeDir::new(path));
        info!("Serving static files from /static for LocalFS backend.");
    }

    let http_listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    info!(
        "HTTP server listening on {}",
        http_listener.local_addr().unwrap()
    );

    // Apply the state to the router and start serving
    axum::serve(http_listener, app.with_state(app_state))
        .await
        .unwrap();

    Ok(())
}

// --- Telemetry Initialization Functions ---
async fn init_telemetry() -> Result<(), Box<dyn std::error::Error>> {
    // Tracing (logging) with JSON format
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .with(tracing_subscriber::fmt::layer().json()) // Output logs as JSON
        .init();

    let otlp_exporter = SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .build()?;

    let tracer_provider = opentelemetry_otlp::SpanExporter::builder()
        .tracing()
        .with_exporter(
            new_exporter()
                .tonic()
                .with_endpoint(format!("http://{}", "0.0.0.0:4317")) // Default OTLP endpoint
                .with_metadata(metadata)
        )
        .install_batch(opentelemetry_sdk::runtime::Tokio)
        .expect("failed to install");

    let tracer = tracer_provider.tracer("image-resizer");
    global::set_tracer_provider(tracer_provider);
    global::set_text_map_propagator(TraceContextPropagator::new()); // Enable trace context propagation
}

fn register_metrics() {
    // Register common metrics at startup
    describe_counter!(
        "image_resize_requests_total",
        "Total number of image resize requests received"
    );
    describe_counter!(
        "image_resize_errors_total",
        "Total number of errors during image resize requests"
    );
    describe_counter!(
        "image_cache_hits_total",
        "Total number of image resize cache hits"
    );
    describe_counter!(
        "image_cache_misses_total",
        "Total number of image resize cache misses"
    );
    describe_histogram!(
        "image_download_duration_seconds",
        "Duration of image download in seconds"
    );
    describe_histogram!(
        "image_process_duration_seconds",
        "Duration of image processing in seconds"
    );
    describe_histogram!(
        "image_upload_duration_seconds",
        "Duration of image upload in seconds"
    );
    describe_gauge!(
        "active_requests",
        "Number of currently active image resize requests"
    );
}

async fn metrics_handler() -> impl IntoResponse {
    let builder = PrometheusBuilder::new();
    let recorder = builder.build_recorder();
    metrics::set_boxed_recorder(Box::new(recorder)).unwrap();

    let metrics_string = recorder.render();
    (StatusCode::OK, [(header::CONTENT_TYPE, "text/plain; charset=utf-8")], metrics_string)
}

// --- 6. The Main Resize Handler ---
#[instrument(skip(state), fields(request_id = %uuid::Uuid::new_v4()))]
async fn resize_handler(
    State(state): State<AppState>,
    Query(mut params): Query<ResizeParams>, // `mut` to cap dimensions
) -> Result<Redirect, AppError> {
    counter!("image_resize_requests_total").increment(1); // Increment total requests counter
    gauge!("active_requests").increment(1); // Increment active requests

    let _span = span!(Level::INFO, "resize_request", ?params).entered();

    // Cap requested dimensions to configured maximums
    params.width = params.width.min(state.max_image_width);
    params.height = params.height.min(state.max_image_height);

    // 6.1. Input Validation
    let validation_span = span!(Level::DEBUG, "input_validation").entered();
    params.validate()?; // Perform validation after capping
    drop(validation_span);
    debug!("Input parameters validated (capped): {:?}", params);

    // 6.2. Generate Cache Key (Deterministic ID for processed image)
    // The cache key is generated from the original URL and requested parameters
    // to ensure unique identification for a specific processed image.
    let cache_key = {
        let mut hasher = Sha256::new();
        hasher.update(params.url.as_bytes());
        hasher.update(params.width.to_string().as_bytes());
        hasher.update(params.height.to_string().as_bytes());
        hasher.update(params.format.to_lowercase().as_bytes());
        let result = hasher.finalize();
        format!("{}.{}", hex::encode(result), params.format.to_lowercase())
    };
    debug!("Generated cache key: {}", cache_key);

    // 6.3. Check Cache (Delegated to StorageBackend)
    let check_cache_span = span!(Level::DEBUG, "check_cache").entered();
    match state.storage.check_cache(&cache_key).await {
        Ok(true) => {
            info!("Cache hit for key: {}", cache_key);
            counter!("image_cache_hits_total").increment(1);
            gauge!("active_requests").decrement(1); // Decrement active requests
            let cdn_url = format!("{}/{}", state.cdn_base_url.trim_end_matches('/'), cache_key);
            return Ok(Redirect::to(&cdn_url));
        }
        Ok(false) => {
            info!(
                "Cache miss for key: {}. Proceeding with processing.",
                cache_key
            );
            counter!("image_cache_misses_total").increment(1);
        }
        Err(e) => {
            // Log error but proceed as if it's a cache miss, to avoid failing on transient storage issues
            error!("Error checking cache for key {}: {:?}", cache_key, e);
            counter!("image_cache_misses_total").increment(1);
        }
    }
    drop(check_cache_span); // End check_cache span

    // 6.4. Download Image
    let download_timer = std::time::Instant::now();
    let download_span = span!(Level::DEBUG, "download_image", url = %params.url).entered();
    let response = reqwest::get(&params.url)
        .await
        .context("Failed to initiate image download")?;
    if !response.status().is_success() {
        let status = response.status();
        let error_msg = format!(
            "Failed to download image from {}: status {}",
            params.url, status
        );
        error!("{}", error_msg);
        counter!("image_resize_errors_total", "type" => "download_failed").increment(1);
        gauge!("active_requests").decrement(1); // Decrement active requests
        return Err(AppError::Anyhow(anyhow::anyhow!(error_msg)));
    }
    let image_bytes = response
        .bytes()
        .await
        .context("Failed to read image bytes")?;
    drop(download_span);
    histogram!("image_download_duration_seconds").record(download_timer.elapsed().as_secs_f64());
    info!("Image downloaded, {} bytes", image_bytes.len());

    // 6.5. Decode and Process Image
    let process_timer = std::time::Instant::now();
    let process_span = span!(Level::DEBUG, "process_image").entered();
    let img = image::load_from_memory(&image_bytes).context("Failed to decode image")?;
    debug!(
        "Image decoded. Original dimensions: {}x{}",
        img.width(),
        img.height()
    );

    // Use `thumbnail` to preserve aspect ratio and fit within dimensions
    let resized_img = img.thumbnail(params.width, params.height);
    // Alternative: `resize_to_fill` to fill the dimensions and crop if aspect ratios differ
    // let resized_img = img.resize_to_fill(params.width, params.height, FilterType::Lanczos3);
    debug!(
        "Image resized to: {}x{}",
        resized_img.width(),
        resized_img.height()
    );

    // 6.6. Encode image to target format
    let mut output_bytes = Cursor::new(Vec::new());
    let (output_format, content_type) = match params.format.to_lowercase().as_str() {
        "jpeg" | "jpg" => (ImageFormat::Jpeg, "image/jpeg"),
        "png" => (ImageFormat::Png, "image/png"),
        "webp" => (ImageFormat::WebP, "image/webp"),
        "gif" => (ImageFormat::Gif, "image/gif"),
        "bmp" => (ImageFormat::Bmp, "image/bmp"),
        "tiff" => (ImageFormat::Tiff, "image/tiff"),
        _ => {
            // This case should ideally be caught by `validate_format` earlier
            counter!("image_resize_errors_total", "type" => "invalid_format_encoding").increment(1);
            gauge!("active_requests").decrement(1); // Decrement active requests
            return Err(AppError::Anyhow(anyhow::anyhow!(
                "Invalid output image format specified for encoding"
            )));
        }
    };

    resized_img
        .write_to(&mut output_bytes, output_format)
        .context(format!("Failed to encode image to {:?}", output_format))?;
    let final_image_bytes = output_bytes.into_inner();
    drop(process_span);
    histogram!("image_process_duration_seconds").record(process_timer.elapsed().as_secs_f64());
    info!(
        "Image encoded to {}, {} bytes",
        params.format,
        final_image_bytes.len()
    );

    // 6.7. Upload to Storage (Delegated to StorageBackend)
    let upload_timer = std::time::Instant::now();
    let upload_span = span!(Level::DEBUG, "upload_to_storage", key = %cache_key).entered();
    info!(
        "Uploading processed image to storage backend with key: {}",
        cache_key
    );
    state
        .storage
        .upload_image(&cache_key, content_type, final_image_bytes)
        .await
        .map_err(|e| {
            counter!("image_resize_errors_total", "type" => "storage_upload_failed").increment(1);
            e // AppError already provides context
        })
        .context("Failed to upload image to storage backend")?; // Add context from Anyhow
    drop(upload_span);
    histogram!("image_upload_duration_seconds").record(upload_timer.elapsed().as_secs_f64());
    info!("Upload successful");

    // 6.8. Construct CDN URL and Redirect
    let cdn_url = format!("{}/{}", state.cdn_base_url.trim_end_matches('/'), cache_key);
    info!("Redirecting to CDN URL: {}", cdn_url);
    gauge!("active_requests").decrement(1); // Decrement active requests

    Ok(Redirect::to(&cdn_url))
}