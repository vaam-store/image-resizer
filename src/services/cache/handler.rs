use crate::models::params::ResizeQuery;
use derive_builder::Builder;
use sha2::{Digest, Sha256};

#[derive(Clone, Builder)]
pub struct CacheService {
    minio_sub_path: String,
}

impl CacheService {
    /// Generate a deterministic cache key based on resize parameters
    pub fn generate_key(&self, params: &ResizeQuery) -> String {
        let mut hasher = Sha256::new();
        hasher.update(params.url.as_bytes());

        match params.width {
            Some(width) => {
                hasher.update(width.to_string().as_bytes());
            }
            None => {
                hasher.update("None".as_bytes());
            }
        }

        match params.height {
            Some(height) => {
                hasher.update(height.to_string().as_bytes());
            }
            None => {
                hasher.update("None".as_bytes());
            }
        }

        hasher.update(params.format.to_string().to_lowercase().as_bytes());

        match params.blur_sigma {
            Some(blur_sigma) => {
                hasher.update(blur_sigma.to_string().as_bytes());
            }
            None => {
                hasher.update("None".as_bytes());
            }
        }

        match params.grayscale {
            Some(grayscale) => {
                hasher.update(grayscale.to_string().as_bytes());
            }
            None => {
                hasher.update("None".as_bytes());
            }
        }

        let result = hasher.finalize();
        format!("{:}{:x}.{}", self.minio_sub_path, result, params.format)
    }
}
