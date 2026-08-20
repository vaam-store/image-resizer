use std::env;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{CACHE_CONTROL, ETAG, IF_NONE_MATCH, LAST_MODIFIED, VARY};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use tokio::sync::Semaphore;
use tower_governor::GovernorLayer;
use tower_governor::governor::GovernorConfigBuilder;
use tower_http::compression::predicate::{NotForContentType, SizeAbove};
use tower_http::compression::{CompressionLayer, DefaultPredicate, Predicate};
use tower_http::cors::{Any, CorsLayer};
use tracing::debug;

use crate::modules::utils::date::immutable_resource_last_modified;
use crate::modules::utils::etag::if_none_match_satisfied;

/// Configurable saturation-shedding and rate-limiting knobs for the router
/// (#43). All read directly from the process environment:
/// `src/modules/env/env.rs` is owned by another agent for this change, so
/// these live here rather than as fields on `EnvConfig` - see the final
/// report for the exact variable names another agent may want to fold in
/// there later.
#[derive(Debug, Clone, Copy)]
pub struct MiddlewareConfig {
    /// Hard ceiling on how long a request may take end-to-end before the
    /// service sheds it with `503` rather than let a slow upstream (or a
    /// wedged cache/storage call) hold the connection indefinitely.
    /// `REQUEST_TIMEOUT_SECS`, default 30s.
    pub request_timeout: Duration,
    /// Maximum number of requests handled concurrently. Beyond this the
    /// service returns `503` immediately rather than queue (matching #30's
    /// convention, already used for the download semaphore's own
    /// saturation response). `MAX_CONCURRENT_REQUESTS`, default 512.
    pub max_concurrent_requests: usize,
    /// Per-IP token-bucket burst size before rate limiting kicks in.
    /// `RATE_LIMIT_BURST`, default 20.
    pub rate_limit_burst: u32,
    /// Per-IP token-bucket replenishment period - one additional request is
    /// allowed roughly every this often, once the burst is exhausted.
    /// `RATE_LIMIT_PERIOD_MS`, default 100ms (10 req/s sustained per IP).
    pub rate_limit_period: Duration,
}

impl Default for MiddlewareConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(30),
            max_concurrent_requests: 512,
            rate_limit_burst: 20,
            rate_limit_period: Duration::from_millis(100),
        }
    }
}

impl MiddlewareConfig {
    pub fn from_env() -> Self {
        let default = Self::default();
        let config = Self {
            request_timeout: env_positive::<u64>("REQUEST_TIMEOUT_SECS")
                .map(Duration::from_secs)
                .unwrap_or(default.request_timeout),
            max_concurrent_requests: env_positive("MAX_CONCURRENT_REQUESTS")
                .unwrap_or(default.max_concurrent_requests),
            rate_limit_burst: env_positive("RATE_LIMIT_BURST").unwrap_or(default.rate_limit_burst),
            rate_limit_period: env_positive::<u64>("RATE_LIMIT_PERIOD_MS")
                .map(Duration::from_millis)
                .unwrap_or(default.rate_limit_period),
        };
        debug!(?config, "Router saturation/rate-limit configuration");
        config
    }
}

/// Parses `name` from the environment as `T`, discarding zero values (a `0`
/// timeout/limit/burst is never a sensible override - treat it the same as
/// unset rather than let it produce a permanently-saturated or
/// permanently-501 router).
fn env_positive<T>(name: &str) -> Option<T>
where
    T: std::str::FromStr + Default + PartialEq,
{
    env::var(name)
        .ok()
        .and_then(|v| v.parse::<T>().ok())
        .filter(|v| *v != T::default())
}

/// Shared state for the concurrency-limit + timeout middleware.
#[derive(Clone)]
struct SaturationGuard {
    semaphore: Arc<Semaphore>,
    timeout: Duration,
}

fn service_unavailable(msg: &'static str) -> Response {
    let mut response = (StatusCode::SERVICE_UNAVAILABLE, msg).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Sheds load with `503` instead of queueing (#43): a non-blocking
/// `try_acquire` on a fixed-size semaphore stands in for
/// `tower::limit::ConcurrencyLimitLayer` + `tower::load_shed::LoadShedLayer`
/// (this workspace's `Cargo.toml` has neither `tower` itself nor
/// tower-http's `limit`/`load-shed` features enabled, and editing the
/// manifest is out of scope for this change - see the final report), and
/// `tokio::time::timeout` stands in for `tower_http::timeout::TimeoutLayer`
/// (gated behind tower-http's `timeout` feature, also not enabled). Both
/// `axum` and `tokio` are already direct dependencies, so this needs
/// neither.
async fn saturation_and_timeout_middleware(
    State(guard): State<SaturationGuard>,
    request: Request,
    next: Next,
) -> Response {
    let permit = match guard.semaphore.try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => return service_unavailable("service is at capacity, try again shortly"),
    };

    let response = match tokio::time::timeout(guard.timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => service_unavailable("request exceeded the configured timeout"),
    };

    drop(permit);
    response
}

/// Path prefix of the single-object download route
/// (`/api/images/files/{key}`, registered by `src/modules/router/router.rs`)
/// that conditional requests (#44) apply to.
const DOWNLOAD_PATH_PREFIX: &str = "/api/images/files/";

/// Serves conditional-GET revalidation for the download endpoint (#44):
/// computes a strong `ETag` directly from the requested cache key (see
/// [`etag_for_request`] for why that's sound without touching storage), and
/// short-circuits to `304 Not Modified` on a matching `If-None-Match`
/// *before* the request ever reaches the real handler - so a revalidation
/// never pays for a storage fetch. Scoped to `/api/images/files/{key}` by
/// path prefix since it runs as a blanket layer over the whole router.
async fn conditional_download_middleware(request: Request, next: Next) -> Response {
    let Some(etag) = etag_for_request(&request) else {
        return next.run(request).await;
    };

    if let Some(if_none_match) = request
        .headers()
        .get(IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        && if_none_match_satisfied(if_none_match, &etag)
    {
        return not_modified_response(&etag);
    }

    let mut response = next.run(request).await;

    if response.status() == StatusCode::OK {
        apply_conditional_headers(response.headers_mut(), &etag);
    }

    response
}

/// The cache key in the path (`<64-hex-sha256>.<ext>` -
/// `CacheService::generate_key`, `src/services/cache/handler.rs`) is a hash
/// of the *resize parameters*, not the image bytes - but it's still a sound
/// strong validator: `StorageService::upload_image` writes each key exactly
/// once and the response is served
/// `Cache-Control: public, max-age=31536000, immutable`, so "same key" and
/// "byte-identical response" are equivalent for the lifetime of the
/// process. Quoted per RFC 7232 §2.3.
fn etag_for_request(request: &Request) -> Option<String> {
    request
        .uri()
        .path()
        .strip_prefix(DOWNLOAD_PATH_PREFIX)
        .filter(|key| !key.is_empty())
        .map(|key| format!("\"{key}\""))
}

fn apply_conditional_headers(headers: &mut HeaderMap, etag: &str) {
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(ETAG, value);
    }
    headers.insert(
        LAST_MODIFIED,
        HeaderValue::from_str(&immutable_resource_last_modified())
            .unwrap_or_else(|_| HeaderValue::from_static("Thu, 01 Jan 1970 00:00:00 GMT")),
    );
    // Real per-`Accept` content negotiation isn't implemented for this
    // route today (`download_handler` in `src/modules/api/download.rs`
    // ignores the request's `Accept` header entirely - the served format is
    // fixed by the cache key's own extension), but this response can be
    // `image/jpeg`, `image/png`, or `image/webp` depending on that key, and
    // a shared cache keyed on URL alone has no other signal that the
    // representation could ever differ per requester. `Vary: Accept` costs
    // nothing today and is exactly the header a future real negotiation
    // would need.
    headers.insert(VARY, HeaderValue::from_static("Accept"));
}

/// RFC 7232 §4.1: a `304` should carry the same cache-related headers
/// (`ETag`, `Vary`, `Cache-Control`) the `200` would have.
fn not_modified_response(etag: &str) -> Response {
    let mut response = Response::builder()
        .status(StatusCode::NOT_MODIFIED)
        .body(Body::empty())
        .expect("empty body with a fixed valid status is always a valid response");
    apply_conditional_headers(response.headers_mut(), etag);
    response.headers_mut().insert(
        CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    response
}

#[inline]
pub fn apply_common_middlewares(router: Router, config: MiddlewareConfig) -> Router {
    let cors = CorsLayer::new()
        // Every registered route (`/api/images/files/{key}`, the signed
        // `/{signature}/{*rest}` resize route, `/health`[, `/metrics`]) is a
        // `GET` - advertising `PUT`/`DELETE`/`PATCH`/`POST` was needless
        // surface (#43): they'd 405 anyway, but there's no reason to
        // advertise methods nothing here accepts.
        .allow_methods([Method::GET, Method::OPTIONS])
        // allow requests from any origin
        .allow_origin(Any);

    let compression_predicate = DefaultPredicate::new()
        .and(NotForContentType::new("application/octet-stream"))
        .and(SizeAbove::new(0));

    let compression_layer = CompressionLayer::new()
        .br(true)
        .deflate(true)
        .gzip(true)
        .zstd(true)
        .compress_when(compression_predicate);

    let saturation_guard = SaturationGuard {
        semaphore: Arc::new(Semaphore::new(config.max_concurrent_requests)),
        timeout: config.request_timeout,
    };

    let governor_config = Arc::new(
        GovernorConfigBuilder::default()
            .period(config.rate_limit_period)
            .burst_size(config.rate_limit_burst)
            .finish()
            .expect(
                "rate-limit period and burst size are validated non-zero by env_positive/Default",
            ),
    );

    // governor's per-key state never shrinks on its own - without periodic
    // pruning, one entry accumulates per distinct client IP for the life of
    // the process. Same cleanup pattern tower-governor's own docs
    // recommend.
    let limiter = governor_config.limiter().clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(60));
        loop {
            interval.tick().await;
            limiter.retain_recent();
        }
    });

    // Request flow (outer -> inner): CORS answers preflight and stamps
    // headers on every response including sheds/errors; the governor rate
    // limiter rejects abusive IPs before any real work; the conditional
    // middleware can resolve a revalidation with a 304 without ever
    // touching the concurrency permit or timeout budget below it;
    // saturation/timeout guards the real (cache-miss) work; compression
    // sits closest to the app since a 304's empty body makes it a no-op
    // anyway.
    router
        .layer(compression_layer)
        .layer(middleware::from_fn_with_state(
            saturation_guard,
            saturation_and_timeout_middleware,
        ))
        .layer(middleware::from_fn(conditional_download_middleware))
        .layer(GovernorLayer::new(governor_config))
        .layer(cors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Json;
    use axum::extract::Path;
    use axum::routing::get;
    use serde_json::json;
    use std::net::SocketAddr;
    use tokio::net::TcpListener;

    async fn spawn_test_router(router: Router) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        // Mirrors `src/main.rs`'s real server: `PeerIpKeyExtractor` (the
        // governor rate limiter, #43) needs `ConnectInfo<SocketAddr>` in
        // each request's extensions, which only
        // `into_make_service_with_connect_info` provides - a plain
        // `into_make_service()` here would make every request fail key
        // extraction (`GovernorError::UnableToExtractKey`, mapped to a
        // misleading `500`) regardless of what this test is actually
        // trying to exercise.
        tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await;
        });
        addr
    }

    // -- #43: saturated concurrency limiter sheds with 503 -----------------

    async fn slow_handler() -> &'static str {
        tokio::time::sleep(Duration::from_millis(250)).await;
        "ok"
    }

    #[tokio::test]
    async fn saturated_concurrency_limiter_returns_503_instead_of_queueing() {
        let app = Router::new().route("/slow", get(slow_handler));
        let app = apply_common_middlewares(
            app,
            MiddlewareConfig {
                request_timeout: Duration::from_secs(5),
                max_concurrent_requests: 1,
                rate_limit_burst: 1000,
                rate_limit_period: Duration::from_millis(1),
            },
        );
        let addr = spawn_test_router(app).await;
        let client = reqwest::Client::new();
        let url = format!("http://{addr}/slow");

        // First request takes (and holds) the sole permit for 250ms.
        let first = {
            let client = client.clone();
            let url = url.clone();
            tokio::spawn(async move { client.get(url).send().await })
        };

        // Give the first request time to actually enter the handler and
        // acquire the permit before firing the second.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let second = client
            .get(&url)
            .send()
            .await
            .expect("second request completes");
        assert_eq!(
            second.status(),
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            "request beyond the concurrency limit must be shed with 503, not queued"
        );

        let first = first.await.expect("join").expect("first request completes");
        assert_eq!(
            first.status(),
            reqwest::StatusCode::OK,
            "the in-flight request holding the permit must still complete successfully"
        );
    }

    #[tokio::test]
    async fn request_exceeding_timeout_returns_503() {
        let app = Router::new().route("/slow", get(slow_handler));
        let app = apply_common_middlewares(
            app,
            MiddlewareConfig {
                request_timeout: Duration::from_millis(50),
                max_concurrent_requests: 10,
                rate_limit_burst: 1000,
                rate_limit_period: Duration::from_millis(1),
            },
        );
        let addr = spawn_test_router(app).await;
        let client = reqwest::Client::new();

        let response = client
            .get(format!("http://{addr}/slow"))
            .send()
            .await
            .expect("request completes (with an error status, not a transport failure)");

        assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    }

    // -- #44: conditional requests -----------------------------------------

    async fn download_stub(Path(key): Path<String>) -> impl axum::response::IntoResponse {
        Json(json!({ "key": key }))
    }

    fn conditional_test_router() -> Router {
        let app = Router::new().route("/api/images/files/{key}", get(download_stub));
        app.layer(middleware::from_fn(conditional_download_middleware))
    }

    #[tokio::test]
    async fn first_request_gets_200_with_etag_last_modified_and_vary() {
        let app = conditional_test_router();
        let addr = spawn_test_router(app).await;
        let client = reqwest::Client::new();
        let key = "abc123.png";

        let response = client
            .get(format!("http://{addr}/api/images/files/{key}"))
            .send()
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(
            response.headers().get("etag").unwrap().to_str().unwrap(),
            format!("\"{key}\"")
        );
        assert_eq!(
            response.headers().get("last-modified").unwrap(),
            "Thu, 01 Jan 1970 00:00:00 GMT"
        );
        assert_eq!(response.headers().get("vary").unwrap(), "Accept");
    }

    #[tokio::test]
    async fn matching_if_none_match_returns_304_with_no_body() {
        let app = conditional_test_router();
        let addr = spawn_test_router(app).await;
        let client = reqwest::Client::new();
        let key = "def456.webp";
        let url = format!("http://{addr}/api/images/files/{key}");

        let first = client
            .get(&url)
            .send()
            .await
            .expect("first request succeeds");
        let etag = first
            .headers()
            .get("etag")
            .expect("first response carries an ETag")
            .to_str()
            .unwrap()
            .to_string();

        let revalidation = client
            .get(&url)
            .header("If-None-Match", &etag)
            .send()
            .await
            .expect("revalidation request succeeds");

        assert_eq!(revalidation.status(), reqwest::StatusCode::NOT_MODIFIED);
        assert_eq!(
            revalidation
                .headers()
                .get("etag")
                .unwrap()
                .to_str()
                .unwrap(),
            etag
        );
        let body = revalidation.bytes().await.expect("read body");
        assert!(body.is_empty(), "304 must not carry a body");
    }

    #[tokio::test]
    async fn non_matching_if_none_match_still_returns_200() {
        let app = conditional_test_router();
        let addr = spawn_test_router(app).await;
        let client = reqwest::Client::new();
        let key = "ghi789.jpeg";

        let response = client
            .get(format!("http://{addr}/api/images/files/{key}"))
            .header("If-None-Match", "\"some-other-key\"")
            .send()
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    #[tokio::test]
    async fn wildcard_if_none_match_returns_304() {
        let app = conditional_test_router();
        let addr = spawn_test_router(app).await;
        let client = reqwest::Client::new();
        let key = "wildcard-key.png";

        let response = client
            .get(format!("http://{addr}/api/images/files/{key}"))
            .header("If-None-Match", "*")
            .send()
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), reqwest::StatusCode::NOT_MODIFIED);
    }

    #[tokio::test]
    async fn non_download_routes_are_left_untouched() {
        let app = Router::new().route("/health", get(|| async { "ok" }));
        let app = app.layer(middleware::from_fn(conditional_download_middleware));
        let addr = spawn_test_router(app).await;
        let client = reqwest::Client::new();

        let response = client
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .expect("request succeeds");

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert!(response.headers().get("etag").is_none());
    }

    // -- CORS methods (#43) --------------------------------------------------

    #[test]
    fn middleware_config_defaults_are_all_positive() {
        let config = MiddlewareConfig::default();
        assert!(config.request_timeout > Duration::ZERO);
        assert!(config.max_concurrent_requests > 0);
        assert!(config.rate_limit_burst > 0);
        assert!(config.rate_limit_period > Duration::ZERO);
    }
}
