use crate::models::params::ResizeQuery;
use derive_builder::Builder;
use sha2::{Digest, Sha256};

#[derive(Clone, Builder)]
pub struct CacheService {}

impl CacheService {
    /// Generate a deterministic cache key based on resize parameters
    pub fn generate_key(&self, params: &ResizeQuery) -> String {
        let mut hasher = Sha256::new();
        hasher.update(params.url.as_bytes());

        if let Some(width) = params.width {
            hasher.update(width.to_string().as_bytes());
        } else {
            hasher.update("None".as_bytes());
        }

        if let Some(height) = params.height {
            hasher.update(height.to_string().as_bytes());
        } else {
            hasher.update("None".as_bytes());
        }

        hasher.update(params.format.to_string().to_lowercase().as_bytes());

        if let Some(blur_sigma) = params.blur_sigma {
            hasher.update(blur_sigma.to_string().as_bytes());
        } else {
            hasher.update("None".as_bytes());
        }

        if let Some(grayscale) = params.grayscale {
            hasher.update(grayscale.to_string().as_bytes());
        } else {
            hasher.update("None".as_bytes());
        }

        let result = hasher.finalize();
        format!("{:x}.{}", result, params.format)
    }
}
