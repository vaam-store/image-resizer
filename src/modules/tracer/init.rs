use crate::modules::env::env::EnvConfig;
use anyhow::Result;
use axum_otel_metrics::{HttpMetricsLayer, HttpMetricsLayerBuilder, PathSkipper};
use opentelemetry::global;
use opentelemetry::trace::TracerProvider;
use opentelemetry_otlp::{Compression, Protocol, SpanExporter, WithExportConfig, WithTonicConfig};
use opentelemetry_sdk::metrics::SdkMeterProvider;
use opentelemetry_sdk::trace::{
    RandomIdGenerator, Sampler, SdkTracerProvider, TracerProviderBuilder,
};
use opentelemetry_sdk::Resource;
use std::sync::Arc;
use std::time::Duration;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[inline]
fn init_tracer_provider(
    otlp_span_endpoint: String,
    otlp_service_name: String,
) -> Result<SdkTracerProvider> {
    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_compression(Compression::Gzip)
        .with_endpoint(otlp_span_endpoint) // Tempo or OTLP endpoint
        .with_timeout(Duration::from_secs(3))
        .build()?;

    let resource = Resource::builder()
        .with_service_name(otlp_service_name)
        .build();

    let tracer_provider = TracerProviderBuilder::default()
        .with_batch_exporter(exporter)
        .with_sampler(Sampler::AlwaysOn)
        .with_id_generator(RandomIdGenerator::default())
        .with_max_events_per_span(16)
        .with_max_attributes_per_span(16)
        .with_resource(resource)
        .build();

    global::set_tracer_provider(tracer_provider.clone());

    Ok(tracer_provider)
}

#[inline]
fn init_meter_provider(
    otlp_metric_endpoint: String,
    otlp_service_name: String,
) -> Result<SdkMeterProvider> {
    let prometheus_exporter = opentelemetry_prometheus::exporter()
        .with_registry(prometheus::default_registry().clone())
        .build()?;

    let exporter = opentelemetry_otlp::MetricExporter::builder()
        .with_tonic()
        .with_compression(Compression::Gzip)
        .with_endpoint(otlp_metric_endpoint)
        .with_protocol(Protocol::Grpc)
        .with_timeout(Duration::from_secs(3))
        .build()?;

    let reader = opentelemetry_sdk::metrics::PeriodicReader::builder(exporter)
        .with_interval(Duration::from_secs(3))
        .build();

    let meter_provider = SdkMeterProvider::builder()
        .with_reader(prometheus_exporter)
        .with_reader(reader)
        .with_resource(
            Resource::builder()
                .with_service_name(otlp_service_name.clone())
                .build(),
        )
        .build();

    global::set_meter_provider(meter_provider.clone());

    Ok(meter_provider)
}

pub async fn init_tracing(
    EnvConfig {
        otlp_metric_endpoint,
        otlp_span_endpoint,
        otlp_service_name,
        log_level,
        ..
    }: EnvConfig,
) -> Result<(HttpMetricsLayer, SdkTracerProvider, SdkMeterProvider)> {
    let tracer_provider = init_tracer_provider(otlp_span_endpoint, otlp_service_name.clone())?;
    let meter_provider = init_meter_provider(otlp_metric_endpoint, otlp_service_name)?;

    let metrics = HttpMetricsLayerBuilder::default()
        .with_skipper(PathSkipper::new_with_fn(Arc::new(|s: &str| {
            s.starts_with("/health") || s.starts_with("/metrics")
        })))
        .with_provider(meter_provider.clone())
        .build();

    // Set up Tracing with OpenTelemetry
    let tracer = tracer_provider.tracer("emgr");
    let layer = tracing_opentelemetry::layer()
        .with_error_records_to_exceptions(true)
        .with_tracer(tracer);

    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(log_level)) // Adjust log level
        .with(tracing_subscriber::fmt::layer()) // Log to console
        .with(layer)
        .with(tracing_opentelemetry::MetricsLayer::new(
            meter_provider.clone(),
        ))
        .init();

    Ok((metrics, tracer_provider, meter_provider))
}
