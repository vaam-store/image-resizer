//! `GET /api/images/files/{key}` - unsigned by design (#27's signing
//! requirement is about the *processing* route, which accepts an
//! attacker-controlled source URL and burns CPU/network fetching it; this
//! route only ever serves bytes already produced and cached by that route,
//! addressed by a content hash that `key_validation::validate_cache_key`
//! already rejects anything malformed against - see that module's doc
//! comment for why an IDOR/traversal guard there is sufficient on its own).
//! Unchanged in shape and behaviour from before #53, just hand-written
//! instead of generated.

use crate::models::params::{DownloadPathParams, ImageFormat};
use crate::modules::api::handler::ApiService;
use crate::modules::utils::err::AppError;
use axum::extract::{Path, State};
use axum::http::HeaderValue;
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;
use tracing::error;

pub async fn download_handler(
    State(api_service): State<Arc<ApiService>>,
    Path(key): Path<String>,
) -> Response {
    let path_params = DownloadPathParams { key };

    match api_service.resize_service.download(&path_params).await {
        Ok(data) => success_response(&path_params.key, data),
        Err(e) => {
            error!("Failed to download image: {}", e);
            AppError::classify_download_error(e).into_response()
        }
    }
}

/// Builds the `200` response, deriving `Content-Type` from the key's own
/// extension (`<hash>.<jpg|png|webp>` - `key_validation::validate_cache_key`
/// already guarantees this shape before `download` ever returns `Ok`)
/// instead of the generated router's hardcoded `image/png` for every
/// format, which was a real bug (#53 fixes it in passing: a downloaded
/// `.jpg`/`.webp` used to be served with a lying `Content-Type: image/png`).
fn success_response(key: &str, data: Vec<u8>) -> Response {
    let content_type = content_type_for_key(key);

    let mut response = data.into_response();
    let headers = response.headers_mut();
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn content_type_for_key(key: &str) -> &'static str {
    let ext = key.rsplit('.').next().unwrap_or_default();
    match ext.parse::<ImageFormat>() {
        Ok(ImageFormat::Jpg) => "image/jpeg",
        Ok(ImageFormat::Png) => "image/png",
        Ok(ImageFormat::Webp) => "image/webp",
        Err(_) => "application/octet-stream",
    }
}

#[cfg(all(test, feature = "local_fs"))]
mod tests {
    use super::*;
    use crate::modules::api::handler::ApiServiceBuilder;
    use crate::modules::signing::SigningConfig;
    use crate::services::cache::handler::CacheServiceBuilder;
    use crate::services::resize::handler::ResizeService;
    use crate::services::storage::handler::{StorageConfig, StorageService};
    use axum::http::StatusCode;
    use axum::body::to_bytes;
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct TestStorageDir(std::path::PathBuf);

    impl std::ops::Deref for TestStorageDir {
        type Target = std::path::Path;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }

    impl Drop for TestStorageDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn valid_key(seed: &str, ext: &str) -> String {
        let digest = Sha256::digest(seed.as_bytes());
        format!("{:x}.{}", digest, ext)
    }

    fn build_test_api_service() -> (ApiService, StorageService, TestStorageDir) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = TestStorageDir(std::env::temp_dir().join(format!(
            "emgr-download-test-{}-{}",
            std::process::id(),
            id
        )));
        std::fs::create_dir_all(&*dir).expect("create test storage dir");

        let storage_config =
            StorageConfig::new("http://cdn.test".to_string()).with_local_fs_config(&*dir);
        let storage_service = StorageService::new(storage_config).expect("build storage service");
        let seed_storage_service = storage_service.clone();
        let cache_service = CacheServiceBuilder::default()
            .minio_sub_path(String::new())
            .build()
            .expect("build cache service");
        let resize_service = ResizeService::new(storage_service, cache_service)
            .expect("build resize service");

        let api_service = ApiServiceBuilder::default()
            .resize_service(resize_service)
            .signing(SigningConfig {
                key: Vec::new(),
                salt: Vec::new(),
                allow_unsigned: true,
            })
            .build()
            .expect("build api service");

        (api_service, seed_storage_service, dir)
    }

    #[tokio::test]
    async fn download_missing_key_returns_404_not_200() {
        let (api_service, _storage, _dir) = build_test_api_service();
        let key = valid_key("never-uploaded", "png");

        let response = download_handler(State(Arc::new(api_service)), Path(key)).await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn download_success_returns_200_with_body_and_matching_content_type() {
        let (api_service, storage, _dir) = build_test_api_service();
        let bytes = vec![0xffu8, 0xd8, 0xff, 0xe0]; // arbitrary bytes, content doesn't need to decode
        let key = valid_key("present", "jpg");
        storage
            .upload_image(&key, "image/jpeg", bytes.clone())
            .await
            .expect("seed storage via the real upload path");

        let response = download_handler(State(Arc::new(api_service)), Path(key)).await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(CONTENT_TYPE).unwrap(),
            "image/jpeg"
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(body.as_ref(), bytes.as_slice());
    }

    #[test]
    fn content_type_matches_each_known_extension() {
        assert_eq!(content_type_for_key("abc.jpg"), "image/jpeg");
        assert_eq!(content_type_for_key("abc.png"), "image/png");
        assert_eq!(content_type_for_key("abc.webp"), "image/webp");
        assert_eq!(content_type_for_key("abc.bin"), "application/octet-stream");
    }
}
