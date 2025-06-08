use crate::models::params::ResizeQuery;
use crate::services::cache::handler::CacheService;
use crate::services::image::handler::ImageService;
use crate::services::storage::handler::StorageService;
use anyhow::Result;
use derive_builder::Builder;
use gen_server::models::DownloadPathParams;
use std::time::Instant;
use tracing::{debug, error, info, instrument};

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
    pub async fn resize(&self, params: &ResizeQuery) -> Result<String> {
        // Generate cache key
        let cache_key = self.cache_service.generate_key(params);
        debug!("Generated cache key: {}", cache_key);

        // Check cache
        match self.storage_service.check_cache(&cache_key).await {
            Ok(true) => {
                info!("Cache hit for key: {}", cache_key);
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

        // Download image
        let download_timer = Instant::now();
        let image_bytes = match self.image_service.download_image(&params.url).await {
            Ok(bytes) => bytes,
            Err(e) => {
                error!("Failed to download image: {}", e);
                return Err(e);
            }
        };
        debug!("Image download took {:?}", download_timer.elapsed());
        info!("Image downloaded, {} bytes", image_bytes.len());

        // Process image
        let process_timer = Instant::now();
        let (processed_image, content_type) =
            match self.image_service.process_image(&image_bytes, params) {
                Ok(result) => result,
                Err(e) => {
                    error!("Failed to process image: {}", e);
                    return Err(e);
                }
            };
        debug!("Image processing took {:?}", process_timer.elapsed());
        info!("Image processed, {} bytes", processed_image.len());

        // Upload to storage
        let upload_timer = Instant::now();
        if let Err(e) = self
            .storage_service
            .upload_image(&cache_key, &content_type, processed_image)
            .await
        {
            error!("Failed to upload image: {}", e);
            return Err(e);
        }
        debug!("Image upload took {:?}", upload_timer.elapsed());
        info!("Upload successful");

        // Return CDN URL
        let cdn_url = self.storage_service.get_cdn_url(&cache_key);
        info!("Returning CDN URL: {}", cdn_url);

        Ok(cdn_url)
    }

    #[instrument(skip(self), fields(url = %params.key))]
    pub async fn download(&self, params: &DownloadPathParams) -> Result<Vec<u8>> {
        let download_timer = Instant::now();

        // First check if the image exists in the cache
        if !self.storage_service.check_cache(&params.key).await? {
            return Err(anyhow::anyhow!(
                "Image not found in storage: {}",
                params.key
            ));
        }

        // Get the image from storage
        match self.storage_service.get_image(&params.key).await {
            Ok(data) => {
                info!("download successful");
                debug!("Image download took {:?}", download_timer.elapsed());
                Ok(data)
            }
            Err(e) => {
                error!("download failed: {}", e);
                Err(e)
            }
        }
    }
}
