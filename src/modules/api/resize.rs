use crate::models::params::ResizeQuery;
use crate::modules::api::handler::ApiService;
use crate::modules::utils::err::AppError;
use async_trait::async_trait;
use axum::http::Method;
use axum_extra::extract::CookieJar;
use gen_server::apis::images::{DownloadResponse, Images, ResizeResponse};
use gen_server::models::{DownloadPathParams, ResizeQueryParams};
use gen_server::types::ByteArray;
use headers::Host;
use tracing::error;

#[async_trait]
impl Images<AppError> for ApiService {
    async fn download(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        path_params: &DownloadPathParams,
    ) -> Result<DownloadResponse, AppError> {
        let byte_array = self.resize_service.download(path_params).await;

        match byte_array {
            Ok(data) => Ok(DownloadResponse::Status200_OperationPerformedSuccessfully {
                body: ByteArray(data),
                cache_control: Some("public, max-age=31536000, immutable".to_string()),
            }),
            Err(e) => {
                error!("Failed to download image: {}", e);
                Err(AppError::classify_download_error(e))
            }
        }
    }

    async fn resize(
        &self,
        _method: &Method,
        _host: &Host,
        _cookies: &CookieJar,
        query_params: &ResizeQueryParams,
    ) -> Result<ResizeResponse, AppError> {
        let query = ResizeQuery::from(query_params.clone());
        let url = self.resize_service.resize(&query).await;

        match url {
            Ok(url) => Ok(
                ResizeResponse::Status301_TheImageWasResizeAndInTheLocationYou {
                    location: Some(url),
                },
            ),
            Err(e) => {
                error!("Failed to resize image: {}", e);
                // No fallback redirect to the caller-supplied URL here: that
                // was an open redirect from a trusted domain (#25) and, since
                // 301s are cached permanently by browsers regardless of
                // Cache-Control, a transient origin failure would permanently
                // steer that client away from the resizer.
                Err(AppError::classify_resize_error(e))
            }
        }
    }
}

// This test module builds its fixture `ApiService` on top of local_fs
// storage (`StorageConfig::with_local_fs_config`, `#[cfg(feature =
// "local_fs")]` in `src/services/storage/handler.rs`) rather than parameterizing
// over every enabled storage backend, so it only compiles when that feature is
// on - matching how `cargo check --features s3` (without `local_fs`) is run
// for this crate.
#[cfg(all(test, feature = "local_fs"))]
mod tests {
    use super::*;
    use crate::modules::api::handler::ApiServiceBuilder;
    use crate::services::cache::handler::CacheServiceBuilder;
    use crate::services::resize::handler::ResizeService;
    use crate::services::storage::handler::{StorageConfig, StorageService};
    use gen_server::apis::images::{DownloadResponse, Images, ResizeResponse};
    use gen_server::models::{DownloadPathParams, ResizeQueryParams};
    use axum::http::uri::Authority;
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn test_host() -> Host {
        Host::from(Authority::from_static("localhost"))
    }

    /// Build a syntactically valid cache key (`<64 lowercase hex>.<ext>`) -
    /// the exact shape `CacheService::generate_key` produces, and the only
    /// shape `StorageService::{check_cache,get_image}` will accept since
    /// #23's key validation landed. `seed` just varies the digest so
    /// different tests get different (but equally valid) keys.
    fn valid_key(seed: &str, ext: &str) -> String {
        let digest = Sha256::digest(seed.as_bytes());
        format!("{:x}.{}", digest, ext)
    }

    /// Owns a per-test local_fs storage directory under the OS temp dir and
    /// removes it on drop, so repeated test runs don't litter the temp dir.
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

    /// Build an `ApiService` backed by an isolated, per-test local_fs storage
    /// directory. Also returns a standalone `StorageService` handle (cheap:
    /// it's an `Arc` clone) wired to the *same* directory, so tests can seed
    /// files through the real, validated `upload_image` path - the storage
    /// layer now shards on-disk paths and validates key shape (#23, #38),
    /// so poking a raw file into the directory directly would no longer be
    /// found by `download`.
    fn build_test_api_service() -> (ApiService, StorageService, TestStorageDir) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = TestStorageDir(std::env::temp_dir().join(format!(
            "emgr-resize-test-{}-{}",
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
        let performance_config = crate::config::performance::PerformanceConfig {
            // The test image server below speaks plain HTTP/1.1; disable the
            // client's HTTP/2-prior-knowledge default so it doesn't send an
            // h2 preface the test server can't parse.
            enable_http2: false,
            // The test image server binds to 127.0.0.1; the SSRF source
            // guard (#21) blocks loopback destinations by default, so allow
            // it here the same way an operator would via
            // ALLOW_LOOPBACK_SOURCE_ADDRESSES for a legitimate local target.
            allow_loopback_source_addresses: true,
            ..Default::default()
        };
        let resize_service =
            ResizeService::with_config(storage_service, cache_service, performance_config)
                .expect("build resize service");

        let api_service = ApiServiceBuilder::default()
            .resize_service(resize_service)
            .build()
            .expect("build api service");

        (api_service, seed_storage_service, dir)
    }

    /// Encode a tiny valid PNG so `image::load_from_memory` can decode it.
    fn tiny_png_bytes() -> Vec<u8> {
        let img = image::ImageBuffer::from_fn(4, 4, |_, _| image::Rgb([255u8, 0, 0]));
        let dyn_img = image::DynamicImage::ImageRgb8(img);
        let mut buf = std::io::Cursor::new(Vec::new());
        dyn_img
            .write_to(&mut buf, image::ImageFormat::Png)
            .expect("encode test png");
        buf.into_inner()
    }

    /// Spawn a one-shot local HTTP server serving `body` as `image/png`, and
    /// return its URL. Used to exercise the real `resize()` download path
    /// without reaching out to the network.
    async fn spawn_test_image_server(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("local addr");

        tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let _ = socket.read(&mut buf).await;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = socket.write_all(header.as_bytes()).await;
                let _ = socket.write_all(&body).await;
                let _ = socket.shutdown().await;
            }
        });

        format!("http://{}/image.png", addr)
    }

    #[tokio::test]
    async fn download_missing_key_returns_not_found_not_200() {
        let (api_service, _storage, _dir) = build_test_api_service();
        // Validly-shaped key that was simply never uploaded.
        let path_params = DownloadPathParams {
            key: valid_key("never-uploaded", "png"),
        };

        let result = <ApiService as Images<AppError>>::download(
            &api_service,
            &Method::GET,
            &test_host(),
            &CookieJar::new(),
            &path_params,
        )
        .await;

        match result {
            Err(AppError::NotFound(_)) => {}
            other => panic!("expected AppError::NotFound, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn download_success_still_returns_200_with_body() {
        let (api_service, storage, _dir) = build_test_api_service();
        let bytes = tiny_png_bytes();
        let key = valid_key("present", "png");
        storage
            .upload_image(&key, "image/png", bytes.clone())
            .await
            .expect("seed storage via the real upload path");

        let path_params = DownloadPathParams { key };

        let result = <ApiService as Images<AppError>>::download(
            &api_service,
            &Method::GET,
            &test_host(),
            &CookieJar::new(),
            &path_params,
        )
        .await;

        match result {
            Ok(DownloadResponse::Status200_OperationPerformedSuccessfully { body, .. }) => {
                assert_eq!(body.0, bytes);
            }
            other => panic!("expected 200 with body, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn resize_failure_does_not_redirect_to_input_url() {
        let (api_service, _storage, _dir) = build_test_api_service();
        // Not a resolvable URL: reqwest fails at request-build time, so this
        // is fast and deterministic without touching the network.
        let bogus_url = "this is not a url".to_string();

        let query_params = ResizeQueryParams {
            url: bogus_url.clone(),
            width: Some(10),
            height: Some(10),
            format: Some(gen_server::models::ImageFormat::Png),
            blur_sigma: None,
            grayscale: None,
        };

        let result = <ApiService as Images<AppError>>::resize(
            &api_service,
            &Method::GET,
            &test_host(),
            &CookieJar::new(),
            &query_params,
        )
        .await;

        match result {
            Err(_) => {}
            Ok(ResizeResponse::Status301_TheImageWasResizeAndInTheLocationYou { location }) => {
                panic!(
                    "resize failure must not redirect to caller-supplied URL, got redirect to {:?} (input was {:?})",
                    location, bogus_url
                );
            }
        }
    }

    #[tokio::test]
    async fn resize_success_returns_redirect_to_cdn_not_source() {
        let (api_service, _storage, _dir) = build_test_api_service();
        let source_url = spawn_test_image_server(tiny_png_bytes()).await;

        let query_params = ResizeQueryParams {
            url: source_url.clone(),
            width: Some(4),
            height: Some(4),
            format: Some(gen_server::models::ImageFormat::Png),
            blur_sigma: None,
            grayscale: None,
        };

        let result = <ApiService as Images<AppError>>::resize(
            &api_service,
            &Method::GET,
            &test_host(),
            &CookieJar::new(),
            &query_params,
        )
        .await;

        match result {
            Ok(ResizeResponse::Status301_TheImageWasResizeAndInTheLocationYou { location }) => {
                let location = location.expect("location header present on success");
                assert_ne!(
                    location, source_url,
                    "success redirect must point at the CDN-hosted copy, not echo the source URL"
                );
                assert!(location.starts_with("http://cdn.test/"));
            }
            other => panic!("expected successful redirect, got {:?}", other),
        }
    }
}
