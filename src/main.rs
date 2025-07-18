use crate::modules::api::handler::ApiService;
use crate::modules::env::env::EnvConfig;
use crate::modules::router::router::router;

use envconfig::Envconfig;
use std::{net::SocketAddr, sync::Arc};
use tokio::net::TcpListener;
use tracing::{debug, info};

mod config;
mod models;
mod modules;
mod services;

use mimalloc::MiMalloc;

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = EnvConfig::init_from_env()?;

    // Initialize tracing and OpenTelemetry
    #[cfg(feature = "otel")]
    let (metrics, trace_provider, meter_provider) = modules::tracer::init_tracing(config.clone()).await?;

    // Get address to listen on
    let addr = format!("{}:{:?}", config.http_host, config.http_port).parse::<SocketAddr>()?;
    let listener = TcpListener::bind(addr).await?;
    debug!(config.http_port, config.http_host, "Will start");
    debug!(
        config.max_concurrent_downloads,
        config.max_concurrent_processing, 
        config.http_timeout_secs, 
        config.max_image_size_mb, 
        config.cpu_thread_pool_size, 
        config.enable_http2, 
        config.connection_pool_size, 
        config.keep_alive_timeout_secs, 
        config.performance_profile, 
        "Performance configuration"
    );

    let api_service = Arc::new(ApiService::create(config)?);

    #[cfg(feature = "otel")]
    let app = router(metrics, api_service).await?;

    #[cfg(not(feature = "otel"))]
    let app = router(api_service).await?;

    // Start the server
    info!("Server running on http://{:?}", listener.local_addr()?);
    axum::serve(listener, app).await?;

    #[cfg(feature = "otel")]
    {
        // Shutdown the tracer provider
        trace_provider.shutdown()?;
        meter_provider.shutdown()?;
    }
    Ok(())
}
