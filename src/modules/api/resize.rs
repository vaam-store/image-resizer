//! `GET /{signature}/{processing_options}/{plain|base64 source}.{extension}`
//! (#53, #27) - imgproxy-compatible signed URL entry point. Replaces the
//! old `GET /api/images/resize?...` query-parameter route entirely (hard
//! cutover, no compatibility alias): same underlying `ResizeService::resize`
//! call and same "301 redirect to the CDN-hosted result, never to the
//! caller-supplied source" behaviour (#25), just parsed from the signed
//! path instead of query parameters.

use crate::models::params::ResizeQuery;
use crate::modules::api::handler::ApiService;
use crate::modules::negotiation;
use crate::modules::signing::SigningConfig;
use crate::modules::signing::verify::verify_signature;
use crate::modules::url::{self, SignedRequest, UrlParseError};
use crate::modules::utils::err::AppError;
use axum::extract::State;
use axum::http::header::{ACCEPT, CACHE_CONTROL, LOCATION, VARY};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use std::sync::Arc;
use tracing::error;

pub async fn resize_handler(
    State(api_service): State<Arc<ApiService>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    match handle(&api_service, uri.path(), &headers).await {
        Ok((location, negotiated)) => redirect_response(&location, negotiated),
        Err(err) => err.into_response(),
    }
}

/// Returns the resolved CDN location plus whether the output format was
/// content-negotiated (#49, `.auto` extension) - the caller needs to know
/// that to decide whether to add `Vary: Accept` to the response, since only
/// a negotiated result actually varies with the `Accept` header. An
/// explicit `.jpg`/`.png`/`.webp`/`.avif`/`.gif` request's location is fully
/// determined by the URL alone.
async fn handle(
    api_service: &ApiService,
    raw_path: &str,
    headers: &HeaderMap,
) -> Result<(String, bool), AppError> {
    let signed = url::split(raw_path).map_err(url_parse_error)?;

    verify_or_reject(&api_service.signing, &signed)?;

    // #52: presets and the processing-option allowlist are applied here,
    // ahead of the plain grammar parse - see
    // `SignedRequest::parse_with_config`.
    let parsed = signed
        .parse_with_config(&api_service.presets, &api_service.allowed_options)
        .map_err(url_parse_error)?;
    let mut query = parsed.into_resize_query();

    // #49: resolve `.auto` against the request's `Accept` header *before*
    // `query` is handed to `ResizeService`/`CacheService::generate_key` -
    // everything below this point only ever sees a concrete format, exactly
    // like every other request shape.
    let accept = headers.get(ACCEPT).and_then(|v| v.to_str().ok());
    let (resolved_format, negotiated) = negotiation::resolve(query.format, accept);
    query.format = resolved_format;

    let location = perform_resize(api_service, &query).await?;
    Ok((location, negotiated))
}

/// The actual resize call, split out from [`handle`] so tests can exercise
/// it directly against a hand-built [`ResizeQuery`] without needing a real
/// signed path - mirrors how the old `Images::resize` trait method used to
/// be called directly in tests before #53.
async fn perform_resize(api_service: &ApiService, query: &ResizeQuery) -> Result<String, AppError> {
    api_service.resize_service.resize(query).await.map_err(|e| {
        error!("Failed to resize image: {}", e);
        // No fallback redirect to the caller-supplied URL here: that was an
        // open redirect from a trusted domain (#25) and, since 301s are
        // cached permanently by browsers regardless of Cache-Control, a
        // transient origin failure would permanently steer that client away
        // from the resizer.
        AppError::classify_resize_error(e)
    })
}

/// Checks the signature segment against `signing`, refusing the request
/// with `403` if it's missing, wrong, or the `unsigned` escape isn't
/// enabled. Runs *before* [`SignedRequest::parse`] so an unauthenticated
/// caller can't use parse-error content as an oracle for the grammar, and
/// so malformed-but-unsigned spam doesn't pay for full parsing.
fn verify_or_reject(signing: &SigningConfig, signed: &SignedRequest) -> Result<(), AppError> {
    if signed.signature_segment == "unsigned" {
        return if signing.allow_unsigned {
            Ok(())
        } else {
            Err(AppError::Forbidden(
                "unsigned requests are refused; signing is required (#27). Set \
                 ALLOW_UNSIGNED_REQUESTS=true to enable the /unsigned/ escape for local \
                 development."
                    .to_string(),
            ))
        };
    }

    if !signing.enabled() {
        return Err(AppError::Forbidden(
            "no signing key is configured; use /unsigned/... in local development".to_string(),
        ));
    }

    if verify_signature(
        &signing.key,
        &signing.salt,
        &signed.signed_path,
        signed.signature_segment,
    ) {
        Ok(())
    } else {
        Err(AppError::Forbidden("invalid signature".to_string()))
    }
}

fn url_parse_error(err: UrlParseError) -> AppError {
    AppError::BadRequest(err.to_string())
}

/// Builds the `301` redirect to the CDN-hosted result. Hand-built with an
/// explicit `StatusCode::MOVED_PERMANENTLY` rather than
/// `axum::response::Redirect::permanent` (#53) - that helper actually
/// issues a `308 Permanent Redirect`, not `301`, and the old generated
/// `ResizeResponse::Status301_...` response (and the test suite pinning
/// this behaviour) is specifically `301`.
///
/// `negotiated` (#49) adds `Vary: Accept` when `true` - only a
/// content-negotiated `.auto` request's `Location` actually depends on the
/// `Accept` header, so every other (explicit-format) request deliberately
/// leaves `Vary` unset rather than needlessly telling shared caches this
/// otherwise URL-determined redirect varies by a header it doesn't.
fn redirect_response(location: &str, negotiated: bool) -> Response {
    let mut response = (StatusCode::MOVED_PERMANENTLY, ()).into_response();
    let headers = response.headers_mut();
    headers.insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    if negotiated {
        headers.insert(VARY, HeaderValue::from_static("Accept"));
    }
    match HeaderValue::from_str(location) {
        Ok(value) => {
            headers.insert(LOCATION, value);
        }
        Err(_) => {
            // A CDN base URL that can't be a header value would be a
            // deployment misconfiguration (`StorageConfig::cdn_base_url`),
            // not a per-request failure - surface it as a clear 500 rather
            // than silently omitting the Location header.
            return AppError::AnyError(anyhow::anyhow!(
                "generated CDN location {location:?} is not a valid HTTP header value"
            ))
            .into_response();
        }
    }
    response
}

// This test module builds its fixture `ApiService` on top of local_fs
// storage (`StorageConfig::with_local_fs_config`, `#[cfg(feature =
// "local_fs")]` in `src/services/storage/handler.rs`) rather than
// parameterizing over every enabled storage backend, so it only compiles
// when that feature is on - matching how `cargo check --features s3`
// (without `local_fs`) is run for this crate.
#[cfg(all(test, feature = "local_fs"))]
mod tests {
    use super::*;
    use crate::models::params::ImageFormat;
    use crate::modules::api::handler::ApiServiceBuilder;
    use crate::services::cache::handler::CacheServiceBuilder;
    use crate::services::resize::handler::ResizeService;
    use crate::services::storage::handler::{StorageConfig, StorageService};
    use axum::body::to_bytes;
    use axum::http::StatusCode;
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn signing_enabled() -> SigningConfig {
        SigningConfig {
            key: b"test-signing-key".to_vec(),
            salt: b"test-salt".to_vec(),
            allow_unsigned: false,
        }
    }

    fn signing_allow_unsigned() -> SigningConfig {
        SigningConfig {
            key: Vec::new(),
            salt: Vec::new(),
            allow_unsigned: true,
        }
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

    fn build_test_api_service(
        signing: SigningConfig,
    ) -> (ApiService, StorageService, TestStorageDir) {
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
            .signing(signing)
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

    fn query(url: String) -> ResizeQuery {
        ResizeQuery {
            url,
            width: Some(4),
            height: Some(4),
            resize_type: crate::models::params::ResizeType::Fit,
            format: ImageFormat::Png,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn resize_failure_does_not_redirect_to_input_url() {
        let (api_service, _storage, _dir) = build_test_api_service(signing_enabled());
        // Not a resolvable URL: reqwest fails at request-build time, so this
        // is fast and deterministic without touching the network.
        let params = query("this is not a url".to_string());

        let result = perform_resize(&api_service, &params).await;

        assert!(result.is_err(), "expected a resize failure");
    }

    #[tokio::test]
    async fn resize_success_returns_redirect_to_cdn_not_source() {
        let (api_service, _storage, _dir) = build_test_api_service(signing_enabled());
        let source_url = spawn_test_image_server(tiny_png_bytes()).await;
        let params = query(source_url.clone());

        let location = perform_resize(&api_service, &params)
            .await
            .expect("resize should succeed");

        assert_ne!(
            location, source_url,
            "success redirect must point at the CDN-hosted copy, not echo the source URL"
        );
        assert!(location.starts_with("http://cdn.test/"));
    }

    /// #27: a well-formed, correctly-signed URL is accepted end-to-end
    /// through the real HTTP handler (path parse -> verify -> resize ->
    /// redirect), not just at the unit level.
    #[tokio::test]
    async fn valid_signature_is_accepted_end_to_end() {
        let (api_service, _storage, _dir) = build_test_api_service(signing_enabled());
        let source_url = spawn_test_image_server(tiny_png_bytes()).await;
        let encoded = URL_SAFE_NO_PAD.encode(source_url.as_bytes());
        let signed_path = format!("/rs:fill:4:4/{encoded}.png");
        let signature = crate::modules::signing::verify::sign(
            &api_service.signing.key,
            &api_service.signing.salt,
            &signed_path,
        );
        let raw_path = format!("/{signature}{signed_path}");

        let response = resize_handler(
            State(Arc::new(api_service)),
            HeaderMap::new(),
            raw_path.parse::<Uri>().expect("valid uri"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
        let location = response
            .headers()
            .get("location")
            .expect("redirect has a location header");
        assert!(location.to_str().unwrap().starts_with("http://cdn.test/"));
    }

    /// #49: a `.auto` request negotiates its output format from the
    /// `Accept` header and the redirect response carries `Vary: Accept` -
    /// an explicit-format request (the test above) must not get that
    /// header at all, since its `Location` doesn't depend on `Accept`.
    #[tokio::test]
    async fn auto_extension_negotiates_and_sets_vary_header() {
        let (api_service, _storage, _dir) = build_test_api_service(signing_enabled());
        let source_url = spawn_test_image_server(tiny_png_bytes()).await;
        let encoded = URL_SAFE_NO_PAD.encode(source_url.as_bytes());
        let signed_path = format!("/rs:fill:4:4/{encoded}.auto");
        let signature = crate::modules::signing::verify::sign(
            &api_service.signing.key,
            &api_service.signing.salt,
            &signed_path,
        );
        let raw_path = format!("/{signature}{signed_path}");

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ACCEPT,
            HeaderValue::from_static("image/avif,image/webp,image/*;q=0.8"),
        );

        let response = resize_handler(
            State(Arc::new(api_service)),
            headers,
            raw_path.parse::<Uri>().expect("valid uri"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
        let location = response
            .headers()
            .get("location")
            .expect("redirect has a location header")
            .to_str()
            .unwrap();
        assert!(
            location.ends_with(".avif"),
            "expected the AVIF-preferring Accept header to negotiate .avif, got {location:?}"
        );
        assert_eq!(
            response.headers().get(VARY).map(|v| v.to_str().unwrap()),
            Some("Accept"),
            "a negotiated response must carry Vary: Accept"
        );
    }

    /// A request naming an explicit format (not `.auto`) must not carry
    /// `Vary: Accept` - its `Location` is fully determined by the URL, not
    /// by the `Accept` header.
    #[tokio::test]
    async fn explicit_format_request_has_no_vary_header() {
        let (api_service, _storage, _dir) = build_test_api_service(signing_enabled());
        let source_url = spawn_test_image_server(tiny_png_bytes()).await;
        let encoded = URL_SAFE_NO_PAD.encode(source_url.as_bytes());
        let signed_path = format!("/rs:fill:4:4/{encoded}.png");
        let signature = crate::modules::signing::verify::sign(
            &api_service.signing.key,
            &api_service.signing.salt,
            &signed_path,
        );
        let raw_path = format!("/{signature}{signed_path}");

        let response = resize_handler(
            State(Arc::new(api_service)),
            HeaderMap::new(),
            raw_path.parse::<Uri>().expect("valid uri"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
        assert_eq!(response.headers().get(VARY), None);
    }

    /// #27: a tampered signature (correct shape, wrong bytes) must be
    /// rejected with `403`, never processed.
    #[tokio::test]
    async fn tampered_signature_is_rejected_end_to_end() {
        let (api_service, _storage, _dir) = build_test_api_service(signing_enabled());
        let encoded = URL_SAFE_NO_PAD.encode(b"http://example.com/img.png");
        let raw_path = format!("/not-a-real-signature/{encoded}.png");

        let response = resize_handler(
            State(Arc::new(api_service)),
            HeaderMap::new(),
            raw_path.parse::<Uri>().expect("valid uri"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// #27: "Unsigned requests are refused by default" - the literal
    /// `unsigned` escape must not work unless explicitly enabled.
    #[tokio::test]
    async fn unsigned_escape_is_refused_when_not_enabled() {
        let (api_service, _storage, _dir) = build_test_api_service(signing_enabled());
        let encoded = URL_SAFE_NO_PAD.encode(b"http://example.com/img.png");
        let raw_path = format!("/unsigned/{encoded}.png");

        let response = resize_handler(
            State(Arc::new(api_service)),
            HeaderMap::new(),
            raw_path.parse::<Uri>().expect("valid uri"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    /// #27: with `ALLOW_UNSIGNED_REQUESTS=true`, `/unsigned/...` bypasses
    /// verification and the request is processed normally.
    #[tokio::test]
    async fn unsigned_escape_is_accepted_when_enabled() {
        let (api_service, _storage, _dir) = build_test_api_service(signing_allow_unsigned());
        let source_url = spawn_test_image_server(tiny_png_bytes()).await;
        let encoded = URL_SAFE_NO_PAD.encode(source_url.as_bytes());
        let raw_path = format!("/unsigned/rs:fill:4:4/{encoded}.png");

        let response = resize_handler(
            State(Arc::new(api_service)),
            HeaderMap::new(),
            raw_path.parse::<Uri>().expect("valid uri"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
    }

    /// A malformed signed-URL path (missing extension) must surface as
    /// `400`, distinct from a `403` signature failure.
    #[tokio::test]
    async fn malformed_grammar_is_bad_request_not_forbidden() {
        let (api_service, _storage, _dir) = build_test_api_service(signing_allow_unsigned());
        let raw_path = "/unsigned/not-a-valid-source-without-extension".to_string();

        let response = resize_handler(
            State(Arc::new(api_service)),
            HeaderMap::new(),
            raw_path.parse::<Uri>().expect("valid uri"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    /// #59: an `rs:{type}:...` segment naming a resize type this crate
    /// doesn't understand must be rejected with `400` end-to-end through
    /// the real HTTP handler, never silently substituted for a supported
    /// type (which is what happened before #59 - the type was parsed and
    /// then discarded).
    #[tokio::test]
    async fn unknown_resize_type_is_bad_request() {
        let (api_service, _storage, _dir) = build_test_api_service(signing_allow_unsigned());
        let encoded = URL_SAFE_NO_PAD.encode(b"https://example.com/img.jpg" as &[u8]);
        let raw_path = format!("/unsigned/rs:crop:800:600/{encoded}.jpg");

        let response = resize_handler(
            State(Arc::new(api_service)),
            HeaderMap::new(),
            raw_path.parse::<Uri>().expect("valid uri"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn error_response_bodies_are_readable() {
        let (api_service, _storage, _dir) = build_test_api_service(signing_enabled());
        let encoded = URL_SAFE_NO_PAD.encode(b"http://example.com/img.png");
        let raw_path = format!("/bad-signature/{encoded}.png");

        let response = resize_handler(
            State(Arc::new(api_service)),
            HeaderMap::new(),
            raw_path.parse::<Uri>().expect("valid uri"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(!body.is_empty());
    }
}
