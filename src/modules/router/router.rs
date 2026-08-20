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
    let app = build_app(api_service)
        .layer(OtelInResponseLayer::default())
        .layer(OtelAxumLayer::default())
        .layer(metrics)
        .route(
            "/metrics",
            get(crate::services::metrics::handler::metrics_handler),
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
