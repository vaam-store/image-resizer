use crate::modules::api::handler::ApiService;
use crate::modules::env::env::EnvConfig;
use crate::modules::router::router::router;

use envconfig::Envconfig;
use std::{net::SocketAddr, sync::Arc};
use tokio::net::TcpListener;
use tracing::{debug, info};

mod modules;
mod services;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = EnvConfig::init_from_env()?;

    // Initialize tracing and OpenTelemetry
    #[cfg(feature = "otel")]
    let (metrics, provider, meter_provider) =
        crate::modules::tracer::init_tracing(config.clone()).await?;

    // Get address to listen on
    let addr = format!("{}:{:?}", config.http_host, config.http_port).parse::<SocketAddr>()?;
    let listener = TcpListener::bind(addr).await?;
    debug!(config.http_port, config.http_host, "Will start");

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
        provider.shutdown()?;
        meter_provider.shutdown()?;
    }
    Ok(())
}
