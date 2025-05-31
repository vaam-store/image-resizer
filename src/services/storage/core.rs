use async_trait::async_trait;

/// Storage backend trait defining operations for image storage
#[async_trait]
pub trait StorageBackend: Send + Sync + 'static {
    /// Uploads image data to the storage backend with a given key and content type.
    async fn upload_image(
        &self,
        key: &str,
        content_type: &str,
        data: Vec<u8>,
    ) -> anyhow::Result<()>;

    /// Checks if an object with the given key exists in the storage backend.
    async fn check_cache(&self, key: &str) -> anyhow::Result<bool>;

    /// Retrieves image data from the storage backend with a given key.
    async fn get_image(&self, key: &str) -> anyhow::Result<Vec<u8>>;
}
