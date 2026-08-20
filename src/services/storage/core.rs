use async_trait::async_trait;
use std::time::Duration;

/// Storage backend trait defining operations for image storage
#[async_trait]
pub trait StorageBackend: Send + Sync + 'static {
    /// Uploads image data to the storage backend with a given key and
    /// content type, with no expiry.
    ///
    /// Provided in terms of [`Self::upload_image_with_ttl`] (rather than
    /// folding `ttl` into this signature) so every existing external caller
    /// of `upload_image` keeps compiling unchanged (#40) - only the resize
    /// pipeline itself, which decides whether a given entry should carry a
    /// TTL, needs to call the `_with_ttl` variant. A backend only needs to
    /// implement `upload_image_with_ttl`; this default forwards to it with
    /// `ttl = None`.
    async fn upload_image(
        &self,
        key: &str,
        content_type: &str,
        data: Vec<u8>,
    ) -> anyhow::Result<()> {
        self.upload_image_with_ttl(key, content_type, data, None)
            .await
    }

    /// Uploads image data with an optional expiry (#40).
    ///
    /// `ttl = None` means "never expires" - the only behaviour available
    /// before this trait grew a TTL concept, and still the default via
    /// [`Self::upload_image`]. `ttl = Some(d)` means every backend must
    /// treat the object as absent - in both `check_cache` and `get_image`
    /// - once `d` has elapsed since this call returned, without requiring a
    /// separate background sweep: expiry is enforced lazily, on the next
    /// read. This is what makes the `Stale`/`Evicted` states in the cache
    /// lifecycle reachable at all (previously neither had any edge leading
    /// to it).
    async fn upload_image_with_ttl(
        &self,
        key: &str,
        content_type: &str,
        data: Vec<u8>,
        ttl: Option<Duration>,
    ) -> anyhow::Result<()>;

    /// Checks if an object with the given key exists in the storage backend
    /// **and has not expired** (#40): an expired-but-not-yet-purged entry
    /// must report `false` here, exactly like a genuinely absent one.
    async fn check_cache(&self, key: &str) -> anyhow::Result<bool>;

    /// Retrieves image data from the storage backend with a given key. Must
    /// fail the same way for an expired entry as for a missing one (#40) -
    /// see `check_cache`.
    async fn get_image(&self, key: &str) -> anyhow::Result<Vec<u8>>;

    /// Deletes an object, if present (#40). Idempotent: deleting an
    /// already-absent (or already-expired) key still returns `Ok(())` -
    /// "not there" and "removed" are the same end state for a purge, so
    /// callers never need to check existence first to avoid an error.
    async fn delete(&self, key: &str) -> anyhow::Result<()>;
}
