use std::sync::Arc;

use crate::modules::api::handler::ApiService;
use crate::modules::router::middlewares::apply_common_middlewares;
use crate::services::health::handler::health;
use anyhow::Result;
use axum::response::Redirect;
use axum::routing::get;
use axum::Router;
use axum_tracing_opentelemetry::middleware::{OtelAxumLayer, OtelInResponseLayer};
use gen_server::server::new;

#[cfg(feature = "otel")]
pub async fn router(metrics: axum_otel_metrics::HttpMetricsLayer, api_service: Arc<ApiService>) -> Result<Router> {
    // Create the main router
    let app = new(api_service)
        .layer(OtelInResponseLayer::default())
        .layer(OtelAxumLayer::default())
        .layer(metrics);

    // Add health and metrics endpoints
    let app = app
        .route("/", get(|| async { Redirect::permanent("/health") }))
        .route("/health", get(health))
        .route("/metrics", get(crate::services::metrics::handler::metrics_handler));

    let router = apply_common_middlewares(app);
    Ok(router)
}

#[cfg(not(feature = "otel"))]
pub async fn router(api_service: Arc<ApiService>) -> Result<Router> {
    // Create the main router
    let app = new(api_service)
        .layer(OtelInResponseLayer::default())
        .layer(OtelAxumLayer::default());

    // Add health and metrics endpoints
    let app = app
        .route("/", get(|| async { Redirect::permanent("/health") }))
        .route("/health", get(health));

    let router = apply_common_middlewares(app);
    Ok(router)
}
