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
    trace::{RandomIdGenerator, Sampler, SdkTracer, SdkTracerProvider},
    Resource,
};
use tracing::{Level, Metadata, Subscriber};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{filter::filter_fn, registry::LookupSpan, Layer};

/// The spans exported as traces, as `(crate, span name)` pairs.
///
/// Each one covers a single bounded operation, such as a peer connection attempt or request.
/// Zebra's other INFO spans, such as `sync`, `crawl`, and the per-connection and global spans,
/// last as long as the process or a connection, so a trace under them would never end, and
/// every span below them would share their one sampling decision.
const EXPORTED_SPANS: &[(&str, &str)] = &[
    ("zebra_network", "dial"),
    ("zebra_network", "connector"),
    ("zebra_network", "accept_inbound_handshake"),
    ("zebra_network", "msg_as_req"),
    ("zebrad", "inbound"),
    ("zebrad", "mempool"),
    ("zebra_consensus", "checkpoint"),
    ("zebra_rpc", "rpc_request"),
    ("zebra_rpc", "lightwalletd_grpc_request"),
];

/// Returns `true` if the OpenTelemetry layer should see this span or event.
///
/// Spans not in [`EXPORTED_SPANS`] are hidden from the layer, so a span exported under one of
/// them starts its own trace. Events are exported only at WARN and above, attached to the
/// exported span they happen in.
fn is_exported(metadata: &Metadata<'_>) -> bool {
    if metadata.is_event() {
        return *metadata.level() <= Level::WARN;
    }

    let krate = metadata.target().split("::").next().unwrap_or_default();

    EXPORTED_SPANS.contains(&(krate, metadata.name()))
}

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

    let layer = exporting_layer(provider.tracer(service_name.to_owned()));

    Ok((Some(layer), Some(provider)))
}

/// Returns a layer that exports the spans and events [`is_exported`] selects to `tracer`.
fn exporting_layer<S>(tracer: SdkTracer) -> impl Layer<S>
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    // Zebra doesn't read the OpenTelemetry context, so activating it on every span enter, and
    // recording thread and idle time attributes, would only add cost.
    OpenTelemetryLayer::new(tracer)
        .with_threads(false)
        .with_tracked_inactivity(false)
        .with_context_activation(false)
        .with_filter(filter_fn(is_exported).with_max_level_hint(Level::INFO))
}

#[cfg(test)]
mod tests {
    use opentelemetry::trace::SpanId;
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SpanData};
    use tracing::{info, info_span, warn};
    use tracing_subscriber::{layer::SubscriberExt, Registry};

    use super::*;

    /// Runs `work` under a subscriber with only the exporting layer, and returns the exported
    /// spans.
    fn export(sampler: Sampler, work: impl FnOnce()) -> Vec<SpanData> {
        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .with_sampler(sampler)
            .build();
        let subscriber = Registry::default().with(exporting_layer(provider.tracer("test")));

        tracing::subscriber::with_default(subscriber, work);

        provider.force_flush().expect("in-memory export can't fail");
        exporter
            .get_finished_spans()
            .expect("in-memory export can't fail")
    }

    /// Runs `requests` inside process-long spans, like the global span and `sync`.
    fn under_long_lived_spans(requests: impl FnOnce()) {
        let _global = tracing::error_span!("", net = "Main").entered();
        let _sync = tracing::info_span!(target: "zebrad::components::sync", "sync").entered();

        requests();
    }

    fn request() {
        let _req = info_span!(target: "zebra_network::peer::connection", "msg_as_req").entered();
        info!("not exported");
        warn!("exported");
    }

    #[test]
    fn exports_only_operation_spans_as_roots() {
        let spans = export(Sampler::AlwaysOn, || under_long_lived_spans(request));

        let [span] = spans.as_slice() else {
            panic!("expected only the request span, got {spans:?}");
        };
        assert_eq!(span.name, "msg_as_req");
        assert_eq!(span.parent_span_id, SpanId::INVALID);

        let events: Vec<_> = span.events.iter().map(|event| &event.name).collect();
        assert_eq!(events, ["exported"]);
    }

    #[test]
    fn samples_each_operation_separately() {
        const REQUESTS: usize = 1000;

        let spans = export(Sampler::TraceIdRatioBased(0.5), || {
            under_long_lived_spans(|| (0..REQUESTS).for_each(|_| request()))
        });

        // With one decision per process, this would be 0 or 1000. The bounds are about 10
        // standard deviations from the mean, so the test doesn't flake.
        assert!(
            (340..=660).contains(&spans.len()),
            "{} of {REQUESTS} requests were sampled",
            spans.len(),
        );
    }
}
