use derive_builder::Builder;
use gen_server::models::ImageFormat;
use hex;
use sha2::{Digest, Sha256};

#[derive(Clone, Builder)]
pub struct CacheService {}

impl CacheService {
    /// Generate a deterministic cache key based on resize parameters
    pub fn generate_key(&self, url: &str, width: u32, height: u32, format: &ImageFormat) -> String {
        let mut hasher = Sha256::new();
        hasher.update(url.as_bytes());
        hasher.update(width.to_string().as_bytes());
        hasher.update(height.to_string().as_bytes());
        hasher.update(format.clone().to_string().to_lowercase().as_bytes());
        let result = hasher.finalize();
        format!("{}.{}", hex::encode(result), format)
    }
}
