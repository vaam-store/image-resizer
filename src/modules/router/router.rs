use std::sync::Arc;

use crate::modules::api::download::download_handler;
use crate::modules::api::handler::ApiService;
use crate::modules::api::resize::resize_handler;
use crate::modules::router::middlewares::{MiddlewareConfig, apply_common_middlewares};
use crate::services::health::handler::health;
use anyhow::Result;
use axum::Router;
use axum::response::Redirect;
use axum::routing::get;
use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};

/// Hand-written router (#53: replaces `gen_server::server::new`, which used
/// to mount the two OpenAPI-generated routes below). Route shapes are
/// otherwise unchanged from before this rewrite:
/// - `GET /api/images/files/{key}` - unchanged, unsigned download route.
/// - the old `GET /api/images/resize?...` query-parameter route is gone
///   (hard cutover, no alias) - replaced by the imgproxy-compatible signed
///   path route `GET /{signature}/{processing_options}/{source}.{extension}`
///   mounted at the root. axum/matchit prefers a literal path segment
///   (`api`, `health`, `metrics`) over this route's dynamic first segment,
///   so it never shadows the other routes below.
///
/// `/health` is deliberately left unauthenticated (#77, an explicit
/// decision, not an oversight): it's the target of Kubernetes
/// liveness/readiness/startup probes
/// (`helm/serverless/templates/knative-service.yaml`'s `livenessProbe`/
/// `readinessProbe`/`startupProbe`, all pointed at `/health`), which hit it
/// directly via HTTP GET and have no
/// practical way to attach a bearer token or any other secret - a
/// protected `/health` would either need the token baked into the
/// Deployment spec (visible to anyone who can `kubectl describe pod`,
/// which defeats the point) or a second, unauthenticated port just for
/// probes (real option, but a materially bigger change - a second
/// listener, its own shutdown wiring, new Helm/Docker surface - than this
/// endpoint's actual sensitivity justifies: unlike `/metrics`, `health`
/// only ever returns the literal string `"OK"`, no traffic/cache/latency
/// data). If that calculus changes, restricting `/health` at the
/// network layer (an ingress rule or `NetworkPolicy` scoping it to the
/// cluster's own probe traffic) is the right lever, not application-layer
/// auth that would break the probes it exists for.
fn build_app(api_service: Arc<ApiService>) -> Router {
    Router::new()
        .route("/", get(|| async { Redirect::temporary("/health") }))
        .route("/health", get(health))
        .route("/api/images/files/{key}", get(download_handler))
        .route("/{signature}/{*rest}", get(resize_handler))
        .with_state(api_service)
}

#[cfg(feature = "otel")]
pub async fn router(
    metrics: axum_otel_metrics::HttpMetricsLayer,
    api_service: Arc<ApiService>,
) -> Result<Router> {
    // Read through the `Arc` before `build_app` takes ownership of it -
    // cloning the (small, `Clone`) config value, not the `Arc` itself.
    let metrics_auth = api_service.metrics_auth.clone();

    let app = build_app(api_service)
        .layer(OtelInResponseLayer::default())
        .layer(OtelAxumLayer::default())
        .layer(metrics)
        .route(
            "/metrics",
            get(crate::services::metrics::handler::metrics_handler).route_layer(
                axum::middleware::from_fn_with_state(
                    metrics_auth,
                    crate::modules::metrics_auth::require_metrics_auth,
                ),
            ),
        );

    let router = apply_common_middlewares(app, MiddlewareConfig::from_env());
    Ok(router)
}

#[cfg(not(feature = "otel"))]
pub async fn router(api_service: Arc<ApiService>) -> Result<Router> {
    let app = build_app(api_service)
        .layer(OtelInResponseLayer::default())
        .layer(OtelAxumLayer::default());

    let router = apply_common_middlewares(app, MiddlewareConfig::from_env());
    Ok(router)
}

/// Uses the exact `local_fs` feature gate + fixture pattern the
/// `resize_handler`/`download_handler` test modules already use
/// (`src/modules/api/resize.rs`, `src/modules/api/download.rs`) - a real
/// `ApiService`, not a stub, so this exercises the same `build_app`
/// production code path those handlers do.
#[cfg(all(test, feature = "local_fs"))]
mod tests {
    use super::*;
    use crate::modules::api::handler::ApiServiceBuilder;
    use crate::modules::signing::SigningConfig;
    use crate::services::cache::handler::CacheServiceBuilder;
    use crate::services::resize::handler::ResizeService;
    use crate::services::storage::handler::{StorageConfig, StorageService};
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::net::TcpListener;

    /// Owns a per-test local_fs storage directory under the OS temp dir and
    /// removes it on drop, mirroring `resize.rs`'s `TestStorageDir`.
    struct TestStorageDir(std::path::PathBuf);

    impl Drop for TestStorageDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn build_minimal_api_service() -> (ApiService, TestStorageDir) {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = TestStorageDir(std::env::temp_dir().join(format!(
            "emgr-router-health-test-{}-{}",
            std::process::id(),
            id
        )));
        std::fs::create_dir_all(&dir.0).expect("create test storage dir");

        let storage_config =
            StorageConfig::new("http://cdn.test".to_string()).with_local_fs_config(&dir.0);
        let storage_service = StorageService::new(storage_config).expect("build storage service");
        let cache_service = CacheServiceBuilder::default()
            .minio_sub_path(String::new())
            .build()
            .expect("build cache service");
        let resize_service = ResizeService::with_config(
            storage_service,
            cache_service,
            crate::config::performance::PerformanceConfig::default(),
        )
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

        (api_service, dir)
    }

    async fn spawn(router: Router) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, router.into_make_service()).await;
        });
        addr
    }

    /// #77: `/health` must stay reachable with zero credentials - it is
    /// the target of Kubernetes liveness/readiness/startup probes (see the
    /// doc comment on [`build_app`] above), which cannot easily carry a
    /// bearer token. This exercises the real production router
    /// construction (`build_app`), not a hand-rolled stub, so a future
    /// change that accidentally wraps `/health` in an auth layer fails
    /// this test rather than only being caught in a running cluster.
    #[tokio::test]
    async fn health_is_reachable_without_any_authorization_header() {
        let (api_service, _dir) = build_minimal_api_service();
        let app = build_app(Arc::new(api_service));
        let addr = spawn(app).await;

        let response = reqwest::Client::new()
            .get(format!("http://{addr}/health"))
            .send()
            .await
            .expect("request completes");

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(response.text().await.unwrap(), "OK");
    }
}
