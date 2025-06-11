use crate::config::performance::PerformanceConfig;
use crate::models::params::ResizeQuery;
use anyhow::{Context, Result};
use bytes::Bytes;
use derive_builder::Builder;
use image::imageops::FilterType;
use image::{GenericImageView, ImageFormat};
use reqwest::Client;
use std::io::Cursor;
use std::sync::Arc;
use tokio::sync::Semaphore;

#[derive(Clone, Builder)]
pub struct ImageService {
    http_client: Arc<Client>,
    // Limit concurrent downloads to prevent memory exhaustion
    download_semaphore: Arc<Semaphore>,
    // Custom thread pool for CPU-intensive work
    cpu_pool: Arc<rayon::ThreadPool>,
    config: PerformanceConfig,
}

impl ImageService {
    pub fn new() -> Result<Self> {
        Self::with_config(PerformanceConfig::default())
    }

    pub fn with_config(config: PerformanceConfig) -> Result<Self> {
        // Configure HTTP client for optimal performance
        let mut client_builder = Client::builder()
            .pool_max_idle_per_host(config.connection_pool_size)
            .pool_idle_timeout(std::time::Duration::from_secs(30))
            .timeout(config.http_timeout)
            .tcp_keepalive(config.keep_alive_timeout);

        if config.enable_http2 {
            client_builder = client_builder.http2_prior_knowledge();
        }

        let http_client = Arc::new(
            client_builder
                .build()
                .context("Failed to create HTTP client")?,
        );

        // Limit concurrent downloads based on configuration
        let download_semaphore = Arc::new(Semaphore::new(config.max_concurrent_downloads));

        // Create custom thread pool for CPU work
        let cpu_pool_size = config.get_cpu_thread_pool_size();
        let cpu_pool = Arc::new(
            rayon::ThreadPoolBuilder::new()
                .num_threads(cpu_pool_size)
                .thread_name(|i| format!("image-cpu-{}", i))
                .build()
                .context("Failed to create CPU thread pool")?,
        );

        Ok(Self {
            http_client,
            download_semaphore,
            cpu_pool,
            config,
        })
    }

    /// Download an image from a URL with optimizations
    pub async fn download_image(&self, url: &str) -> Result<Vec<u8>> {
        // Acquire semaphore to limit concurrent downloads
        let _permit = self
            .download_semaphore
            .acquire()
            .await
            .context("Failed to acquire download permit")?;

        let response = self
            .http_client
            .get(url)
            .send()
            .await
            .context("Failed to initiate image download")?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to download image from {}: status {}",
                url,
                response.status()
            ));
        }

        // Check content length to prevent downloading huge files
        if let Some(content_length) = response.content_length() {
            if content_length > self.config.max_image_size {
                return Err(anyhow::anyhow!(
                    "Image too large: {} bytes (max: {} bytes)",
                    content_length,
                    self.config.max_image_size
                ));
            }
        }

        // Stream the response body efficiently
        let bytes = response
            .bytes()
            .await
            .context("Failed to read image bytes")?;

        Ok(bytes.to_vec())
    }

    /// Process image using custom thread pool with CPU affinity
    pub async fn process_image(
        &self,
        image_bytes: &[u8],
        params: &ResizeQuery,
    ) -> Result<(Vec<u8>, String)> {
        let image_bytes = Bytes::copy_from_slice(image_bytes);
        let params = params.clone();
        let cpu_pool = Arc::clone(&self.cpu_pool);

        // Use custom thread pool instead of tokio's spawn_blocking
        let (tx, rx) = tokio::sync::oneshot::channel();

        cpu_pool.spawn(move || {
            let result = Self::process_image_blocking(&image_bytes, &params);
            let _ = tx.send(result);
        });

        rx.await.context("Image processing task was cancelled")?
    }

    /// CPU-intensive image processing with optimizations
    fn process_image_blocking(
        image_bytes: &[u8],
        params: &ResizeQuery,
    ) -> Result<(Vec<u8>, String)> {
        // Use faster image decoding with format hints
        let img = if let Some(format) = Self::detect_format_from_bytes(image_bytes) {
            image::load_from_memory_with_format(image_bytes, format)
                .context("Failed to decode image with format hint")?
        } else {
            image::load_from_memory(image_bytes).context("Failed to decode image")?
        };

        // Use faster resize algorithms for different scenarios
        let filter = match (params.width, params.height) {
            // For thumbnails, use faster Triangle filter
            (Some(w), Some(h)) if w <= 300 && h <= 300 => FilterType::Triangle,
            // For high quality, use Lanczos3
            _ => FilterType::Lanczos3,
        };

        // Resize image with optimized logic
        let img = match (params.width, params.height) {
            (Some(w), None) => img.resize(w, u32::MAX, filter),
            (None, Some(h)) => img.resize(u32::MAX, h, filter),
            (Some(w), Some(h)) => {
                // Optimize resize-to-fill + crop operation
                let img = img.resize_to_fill(w, h, filter);
                let (current_width, current_height) = img.dimensions();

                if current_width == w && current_height == h {
                    img // No cropping needed
                } else {
                    let crop_x = (current_width.saturating_sub(w)) / 2;
                    let crop_y = (current_height.saturating_sub(h)) / 2;
                    img.crop_imm(crop_x, crop_y, w.min(current_width), h.min(current_height))
                }
            }
            (None, None) => img,
        };

        // Apply filters efficiently
        let img = if let Some(true) = params.grayscale {
            img.grayscale()
        } else {
            img
        };

        let img = if let Some(sigma) = params.blur_sigma {
            if sigma > 0.0 { img.blur(sigma) } else { img }
        } else {
            img
        };

        // Optimize encoding based on format
        let (output_format, content_type) = match params.format {
            gen_server::models::ImageFormat::Jpg => (ImageFormat::Jpeg, "image/jpeg"),
            gen_server::models::ImageFormat::Png => (ImageFormat::Png, "image/png"),
            gen_server::models::ImageFormat::Webp => (ImageFormat::WebP, "image/webp"),
        };

        // Pre-allocate buffer based on estimated size
        let estimated_size = Self::estimate_output_size(&img, &output_format);
        let mut output_bytes = Cursor::new(Vec::with_capacity(estimated_size));

        img.write_to(&mut output_bytes, output_format)
            .context(format!("Failed to encode image to {:?}", output_format))?;

        Ok((output_bytes.into_inner(), content_type.to_string()))
    }

    /// Detect image format from magic bytes for faster decoding
    fn detect_format_from_bytes(bytes: &[u8]) -> Option<ImageFormat> {
        if bytes.len() < 12 {
            return None;
        }

        match &bytes[0..4] {
            [0xFF, 0xD8, 0xFF, _] => Some(ImageFormat::Jpeg),
            [0x89, 0x50, 0x4E, 0x47] => Some(ImageFormat::Png),
            _ => {
                // Check for WebP
                if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
                    Some(ImageFormat::WebP)
                } else {
                    None
                }
            }
        }
    }

    /// Estimate output buffer size to reduce allocations
    fn estimate_output_size(img: &image::DynamicImage, format: &ImageFormat) -> usize {
        let (width, height) = img.dimensions();
        let pixels = (width * height) as usize;

        match format {
            ImageFormat::Jpeg => pixels / 2, // Rough estimate for JPEG compression
            ImageFormat::Png => pixels * 4,  // RGBA
            ImageFormat::WebP => pixels / 3, // WebP compression estimate
            _ => pixels * 3,                 // Default RGB
        }
    }
}

impl Default for ImageService {
    fn default() -> Self {
        Self::new().expect("Failed to create default ImageService")
    }
}
