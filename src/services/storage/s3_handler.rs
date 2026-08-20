use anyhow::{Context, Result};
use async_trait::async_trait;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use aws_sdk_s3 as s3;
use aws_sdk_s3::operation::get_object::GetObjectError;
use aws_sdk_s3::operation::head_object::HeadObjectError;
use aws_sdk_s3::primitives::{ByteStream, DateTime, DateTimeFormat};

use crate::services::storage::core::StorageBackend;

/// MinIO storage implementation
pub struct MinIOStorage {
    client: s3::Client,
    bucket: String,
}

impl MinIOStorage {
    pub fn new_minio(
        endpoint_url: String,
        access_key: String,
        secret_key: String,
        bucket: String,
        region: String,
    ) -> anyhow::Result<Self> {
        let s3_config = s3::config::Builder::new()
            .endpoint_url(endpoint_url)
            .credentials_provider(s3::config::Credentials::new(
                access_key, secret_key, None,     // session_token
                None,     // expiry
                "Static", // provider_name
            ))
            .region(s3::config::Region::new(region))
            .force_path_style(true) // Crucial for MinIO compatibility
            .build();

        let s3_client = s3::Client::from_conf(s3_config);

        Ok(Self {
            client: s3_client,
            bucket,
        })
    }

    /// Whether an S3 `Expires` header value (as returned by `head_object`/
    /// `get_object`) means the object should be treated as absent (#40).
    /// `None` (no `Expires` set on the object, i.e. `upload_image_with_ttl`
    /// was called with `ttl: None`) means "never expires". Takes the raw
    /// header string (`expires_string()`) rather than the deprecated typed
    /// `expires()` accessor, and fails open (treats unparsable as "not
    /// expired") on a malformed value - same fail-open contract as every
    /// other backend's expiry check.
    fn is_expired(expires: Option<&str>) -> bool {
        let Some(expires) = expires else {
            return false;
        };
        let Ok(expires) = DateTime::from_str(expires, DateTimeFormat::HttpDate) else {
            return false;
        };
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        now_secs >= expires.secs()
    }
}

#[async_trait]
impl StorageBackend for MinIOStorage {
    async fn upload_image_with_ttl(
        &self,
        key: &str,
        content_type: &str,
        data: Vec<u8>,
        ttl: Option<Duration>,
    ) -> Result<()> {
        let mut request = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(data))
            .content_type(content_type)
            // Cache-Control was previously left unset on the object itself;
            // this now matches the header the download response is served
            // with (`src/modules/api/resize.rs`), so it's also correct for
            // anything reading the object directly from S3/MinIO/a CDN
            // fronting the bucket, not just via this app's own endpoint.
            .cache_control("public, max-age=31536000, immutable");

        // TTL concept (#40): set the S3 object's own `Expires` metadata so
        // `check_cache`/`get_image` can lazily evict it on the next read,
        // without a separate background sweep or lifecycle rule to manage.
        if let Some(ttl) = ttl {
            if let Some(expires_at) = SystemTime::now().checked_add(ttl) {
                request = request.expires(DateTime::from(expires_at));
            }
        }

        request
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("S3 error: {}", e))
            .context("Failed to upload image to MinIO")?;
        Ok(())
    }

    async fn check_cache(&self, key: &str) -> Result<bool> {
        match self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(output) => {
                if Self::is_expired(output.expires_string()) {
                    // Lazy eviction (#40): best-effort - if the delete
                    // fails or races another reader, the entry is still
                    // correctly reported as a miss here; the object is just
                    // cleaned up a little later.
                    let _ = self.delete(key).await;
                    return Ok(false);
                }
                Ok(true)
            }
            Err(sdk_err) => match sdk_err.into_service_error() {
                HeadObjectError::NotFound(_) => Ok(false),
                err => Err(anyhow::anyhow!("S3 error: {}", err)),
            },
        }
    }

    async fn get_image(&self, key: &str) -> Result<Vec<u8>> {
        let response = match self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
        {
            Ok(response) => response,
            Err(sdk_err) => {
                return match sdk_err.into_service_error() {
                    // Matched by message content, not type, one layer up
                    // (`AppError::classify_download_error`, `src/modules/utils/err.rs`,
                    // owned separately) - "not found" must appear in the text.
                    GetObjectError::NoSuchKey(_) => {
                        Err(anyhow::anyhow!("Image not found in S3: {}", key))
                    }
                    err => Err(anyhow::anyhow!("S3 error: {}", err))
                        .context(format!("Failed to get image from S3: {}", key)),
                };
            }
        };

        if Self::is_expired(response.expires_string()) {
            // Same lazy-eviction contract as `check_cache` (#40): an
            // expired entry must fail exactly like a missing one.
            let _ = self.delete(key).await;
            return Err(anyhow::anyhow!("Image not found in S3: {} (expired)", key));
        }

        let data = response
            .body
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read S3 response body: {}", e))?;

        Ok(data.into_bytes().to_vec())
    }

    async fn delete(&self, key: &str) -> Result<()> {
        // S3's DeleteObject is itself idempotent - it returns success
        // whether or not the key exists - so no NotFound special-casing is
        // needed here (#40).
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("S3 error: {}", e))
            .context(format!("Failed to delete image from S3: {}", key))?;
        Ok(())
    }
}
