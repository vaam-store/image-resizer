use anyhow::{Context, Result};
use async_trait::async_trait;

use aws_sdk_s3 as s3;
use aws_sdk_s3::operation::head_object::HeadObjectError;
use aws_sdk_s3::primitives::ByteStream;

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
}

#[async_trait]
impl StorageBackend for MinIOStorage {
    async fn upload_image(&self, key: &str, content_type: &str, data: Vec<u8>) -> Result<()> {
        self.client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(data))
            .content_type(content_type)
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
            Ok(_) => Ok(true),
            Err(sdk_err) => match sdk_err.into_service_error() {
                HeadObjectError::NotFound(_) => Ok(false),
                err => Err(anyhow::anyhow!("S3 error: {}", err)),
            },
        }
    }

    async fn get_image(&self, key: &str) -> Result<Vec<u8>> {
        let response = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("S3 error: {}", e))
            .context(format!("Failed to get image from S3: {}", key))?;

        let data = response
            .body
            .collect()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read S3 response body: {}", e))?;

        Ok(data.into_bytes().to_vec())
    }
}
