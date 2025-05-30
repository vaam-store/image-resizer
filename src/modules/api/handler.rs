use crate::services::cache::handler::{CacheService, CacheServiceBuilder};
use crate::services::image::handler::{ImageService, ImageServiceBuilder};
use crate::services::resize::handler::{ResizeService, ResizeServiceBuilder};
use crate::services::storage::handler::{StorageService, StorageServiceBuilder};
use anyhow::Result;
use derive_builder::Builder;
use gen_server::apis::ErrorHandler;
use std::env;
use std::path::PathBuf;

#[derive(Clone, Builder)]
pub struct ApiService {
    pub resize_service: ResizeService,
}

impl ApiService {
    pub fn create() -> Result<Self> {
        // Initialize cache service
        let cache_service = CacheServiceBuilder::default().build()?;

        // Initialize image service
        let max_width: u32 = env::var("MAX_IMAGE_WIDTH")
            .unwrap_or_else(|_| "2000".to_string())
            .parse()
            .unwrap_or(2000);
        let max_height: u32 = env::var("MAX_IMAGE_HEIGHT")
            .unwrap_or_else(|_| "2000".to_string())
            .parse()
            .unwrap_or(2000);

        let image_service = ImageServiceBuilder::default()
            .max_width(max_width)
            .max_height(max_height)
            .build()?;

        // Initialize storage service
        let storage_type = env::var("STORAGE_TYPE")
            .unwrap_or_else(|_| "MINIO".to_string())
            .to_uppercase();

        let cdn_base_url =
            env::var("CDN_BASE_URL").unwrap_or_else(|_| "http://localhost:3000/static".to_string());

        let storage_service = match storage_type.as_str() {
            "MINIO" => {
                let minio_endpoint_url = env::var("MINIO_ENDPOINT_URL")
                    .unwrap_or_else(|_| "http://localhost:9000".to_string());
                let minio_access_key =
                    env::var("MINIO_ACCESS_KEY_ID").unwrap_or_else(|_| "minioadmin".to_string());
                let minio_secret_key = env::var("MINIO_SECRET_ACCESS_KEY")
                    .unwrap_or_else(|_| "minioadmin".to_string());
                let minio_bucket =
                    env::var("MINIO_BUCKET").unwrap_or_else(|_| "images".to_string());
                let minio_region =
                    env::var("MINIO_REGION").unwrap_or_else(|_| "us-east-1".to_string());

                StorageService::new_minio(
                    minio_endpoint_url,
                    minio_access_key,
                    minio_secret_key,
                    minio_bucket,
                    minio_region,
                    cdn_base_url,
                )?
            }
            "LOCAL_FS" => {
                let local_fs_storage_path =
                    env::var("LOCAL_FS_STORAGE_PATH").unwrap_or_else(|_| "./storage".to_string());
                let path = PathBuf::from(local_fs_storage_path);

                StorageService::new_local_fs(path, cdn_base_url)?
            }
            _ => {
                // Default to local storage
                let path = PathBuf::from("./storage");
                StorageService::new_local_fs(path, cdn_base_url)?
            }
        };

        // Initialize resize service
        let resize_service = ResizeServiceBuilder::default()
            .storage_service(storage_service)
            .cache_service(cache_service)
            .image_service(image_service)
            .build()?;

        // Create API service
        let api_service = ApiServiceBuilder::default()
            .resize_service(resize_service)
            .build()?;

        Ok(api_service)
    }
}

impl ErrorHandler<()> for ApiService {}

impl AsRef<ApiService> for ApiService {
    fn as_ref(&self) -> &ApiService {
        self
    }
}
