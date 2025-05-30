use crate::services::cache::handler::CacheService;
use crate::services::image::handler::ImageService;
use crate::services::storage::handler::StorageService;
use anyhow::{Context, Result};
use async_trait::async_trait;
use derive_builder::Builder;
use gen_server::models::{ImageFormat, ResizeAnImageQueryParams};
use std::time::Instant;
use tracing::{debug, error, info, instrument, span, Level};
use validator::Validate;

/// Validation struct for resize parameters
#[derive(Debug, Validate)]
struct ResizeParams {
    #[validate(url)]
    url: String,
    #[validate(range(min = 1))]
    width: u32,
    #[validate(range(min = 1))]
    height: u32,
    format: ImageFormat,
}

/// Main service for image resizing
#[derive(Clone, Builder)]
pub struct ResizeService {
    storage_service: StorageService,
    cache_service: CacheService,
    image_service: ImageService,
}

impl ResizeService {
    /// Main resize method that orchestrates the entire process
    #[instrument(skip(self), fields(url = %params.url))]
    pub async fn resize(&self, params: &ResizeAnImageQueryParams) -> Result<String> {
        // Extract and validate parameters
        let resize_params = ResizeParams {
            url: params.url.clone(),
            width: params.width as u32,
            height: params.height as u32,
            format: params.format,
        };

        // Validate parameters
        resize_params
            .validate()
            .context("Invalid resize parameters")?;

        // Generate cache key
        let cache_key = self.cache_service.generate_key(
            &resize_params.url,
            resize_params.width,
            resize_params.height,
            &resize_params.format,
        );
        debug!("Generated cache key: {}", cache_key);

        // Check cache
        let check_cache_span = span!(Level::DEBUG, "check_cache").entered();
        match self.storage_service.check_cache(&cache_key).await {
            Ok(true) => {
                info!("Cache hit for key: {}", cache_key);
                drop(check_cache_span);
                return Ok(self.storage_service.get_cdn_url(&cache_key));
            }
            Ok(false) => {
                info!(
                    "Cache miss for key: {}. Proceeding with processing.",
                    cache_key
                );
            }
            Err(e) => {
                error!("Error checking cache for key {}: {:?}", cache_key, e);
                // Continue as if it's a cache miss
            }
        }
        drop(check_cache_span);

        // Download image
        let download_timer = Instant::now();
        let download_span =
            span!(Level::DEBUG, "download_image", url = %resize_params.url).entered();
        let image_bytes = match self.image_service.download_image(&resize_params.url).await {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to download image: {}", e);
                return Err(e);
            }
        };
        drop(download_span);
        debug!("Image download took {:?}", download_timer.elapsed());
        info!("Image downloaded, {} bytes", image_bytes.len());

        // Process image
        let process_timer = Instant::now();
        let process_span = span!(Level::DEBUG, "process_image").entered();
        let (processed_image, content_type) = match self.image_service.process_image(
            &image_bytes,
            resize_params.width,
            resize_params.height,
            &resize_params.format,
        ) {
            Ok(result) => result,
            Err(e) => {
                error!("Failed to process image: {}", e);
                return Err(e);
            }
        };
        drop(process_span);
        debug!("Image processing took {:?}", process_timer.elapsed());
        info!("Image processed, {} bytes", processed_image.len());

        // Upload to storage
        let upload_timer = Instant::now();
        let upload_span = span!(Level::DEBUG, "upload_to_storage", key = %cache_key).entered();
        if let Err(e) = self
            .storage_service
            .upload_image(&cache_key, &content_type, processed_image)
            .await
        {
            error!("Failed to upload image: {}", e);
            return Err(e);
        }
        drop(upload_span);
        debug!("Image upload took {:?}", upload_timer.elapsed());
        info!("Upload successful");

        // Return CDN URL
        let cdn_url = self.storage_service.get_cdn_url(&cache_key);
        info!("Returning CDN URL: {}", cdn_url);

        Ok(cdn_url)
    }
}
