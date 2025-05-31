use anyhow::{Context, Result};
use async_trait::async_trait;
use std::path::PathBuf;

use crate::services::storage::core::StorageBackend;

/// Local file system storage implementation
pub struct LocalFSStorage {
    base_path: PathBuf,
}

impl LocalFSStorage {
    pub(crate) fn new(base_path: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            base_path: base_path.into(),
        })
    }
}

#[async_trait]
impl StorageBackend for LocalFSStorage {
    async fn upload_image(&self, key: &str, _content_type: &str, data: Vec<u8>) -> Result<()> {
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

    async fn check_cache(&self, key: &str) -> Result<bool> {
        let file_path = self.base_path.join(key);
        Ok(tokio::fs::metadata(&file_path).await.is_ok())
    }

    async fn get_image(&self, key: &str) -> Result<Vec<u8>> {
        let file_path = self.base_path.join(key);
        tokio::fs::read(&file_path).await.context(format!(
            "Failed to read image from local file system: {}",
            file_path.display()
        ))
    }
}
