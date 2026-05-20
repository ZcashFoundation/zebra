//! OpenTelemetry tracing layer with zero overhead when disabled.
//!
//! This module provides OpenTelemetry distributed tracing support that can be
//! compiled into production builds but only activated at runtime when an
//! endpoint is configured.
//!
//! # Transport
//!
//! Uses HTTP transport with a blocking reqwest client. This works without an
//! async runtime context because:
//! - The BatchSpanProcessor spawns its own dedicated background thread
//! - The reqwest-blocking-client handles HTTP exports synchronously
//!
//! This is important because Zebra's tracing component initializes before the
//! Tokio runtime starts.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{Protocol, WithExportConfig};
use opentelemetry_sdk::{
    trace::{RandomIdGenerator, Sampler, SdkTracerProvider},
    Resource,
};
use tracing::Subscriber;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{registry::LookupSpan, Layer};

/// Error type for OpenTelemetry layer initialization.
pub type OtelError = Box<dyn std::error::Error + Send + Sync + 'static>;

/// Creates an OpenTelemetry layer if an endpoint is configured.
///
/// Returns `(None, None)` with ZERO overhead when endpoint is `None` -
/// no SDK objects are created, no background tasks are spawned.
///
/// # Arguments
///
/// * `endpoint` - OTLP HTTP endpoint URL (e.g., "http://localhost:4318")
/// * `service_name` - Service name for traces (defaults to "zebra")
/// * `sample_percent` - Sampling percentage between 0 and 100 (defaults to 100)
///
/// # Errors
///
/// Returns an error if the OTLP exporter fails to initialize.
pub fn layer<S>(
    endpoint: Option<&str>,
    service_name: Option<&str>,
    sample_percent: Option<u8>,
) -> Result<(Option<impl Layer<S>>, Option<SdkTracerProvider>), OtelError>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    // CRITICAL: Check config FIRST - zero-cost path when None
    let endpoint = match endpoint {
        Some(ep) => ep,
        None => return Ok((None, None)), // No SDK objects created
    };

    // HTTP transport requires the /v1/traces path suffix.
    // Append it if not already present.
    let endpoint = if endpoint.ends_with("/v1/traces") {
        endpoint.to_string()
    } else {
        format!("{}/v1/traces", endpoint.trim_end_matches('/'))
    };

    let service_name = service_name.unwrap_or("zebra");
    // Convert percentage (0-100) to rate (0.0-1.0), clamped to valid range
    let sample_rate = f64::from(sample_percent.unwrap_or(100).min(100)) / 100.0;

    // Build the HTTP exporter with blocking client.
    // This works without an async runtime because:
    // 1. reqwest-blocking-client doesn't need tokio
    // 2. BatchSpanProcessor spawns its own background thread for exports
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(&endpoint)
        .build()?;

    // Use ratio-based sampling for production flexibility
    let sampler = if sample_rate >= 1.0 {
        Sampler::AlwaysOn
    } else if sample_rate <= 0.0 {
        Sampler::AlwaysOff
    } else {
        Sampler::TraceIdRatioBased(sample_rate)
    };

    let resource = Resource::builder()
        .with_service_name(service_name.to_owned())
        .build();

    // Use batch exporter - it spawns its own dedicated background thread
    // for collecting and exporting spans, so it doesn't need an external runtime
    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(sampler)
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource)
        .build();

    let tracer = provider.tracer(service_name.to_owned());
    let layer = OpenTelemetryLayer::new(tracer);

    Ok((Some(layer), Some(provider)))
}
