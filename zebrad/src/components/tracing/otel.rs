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
/// * `sample_percent` - Sampling percentage between 0 and 100. If `None`, the sampler is read
///   from the standard `OTEL_TRACES_SAMPLER` and `OTEL_TRACES_SAMPLER_ARG` environment
///   variables, and every trace is sampled if they are unset.
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

    // Build the HTTP exporter with blocking client.
    // This works without an async runtime because:
    // 1. reqwest-blocking-client doesn't need tokio
    // 2. BatchSpanProcessor spawns its own background thread for exports
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(&endpoint)
        .build()?;

    let resource = Resource::builder()
        .with_service_name(service_name.to_owned())
        .build();

    // Use batch exporter - it spawns its own dedicated background thread
    // for collecting and exporting spans, so it doesn't need an external runtime
    let mut provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(resource);

    // The builder starts from the sampler in the standard environment variables, so only a
    // configured percentage replaces it.
    if let Some(sample_percent) = sample_percent {
        provider = provider.with_sampler(sampler_for_percent(sample_percent));
    }

    let provider = provider.build();

    let layer = exporting_layer(provider.tracer(service_name.to_owned()));

    Ok((Some(layer), Some(provider)))
}

/// Returns the sampler for a percentage of traces, treating values above 100 as 100.
fn sampler_for_percent(sample_percent: u8) -> Sampler {
    match sample_percent {
        0 => Sampler::AlwaysOff,
        100.. => Sampler::AlwaysOn,
        percent => Sampler::TraceIdRatioBased(f64::from(percent) / 100.0),
    }
}

/// Returns the sampler Zebra uses, as the OpenTelemetry SDK describes it.
///
/// A configured percentage replaces the sampler in the standard environment variables.
pub(super) fn sampler_description(sample_percent: Option<u8>) -> String {
    match sample_percent {
        Some(percent) => format!("{:?}", sampler_for_percent(percent)),
        // The SDK reads the environment variables when it builds its default configuration.
        None => format!("{:?}", opentelemetry_sdk::trace::Config::default().sampler),
    }
}

/// Returns why the standard sampler environment variables won't sample as an operator
/// probably intended, if they won't.
///
/// The SDK warns about some of these itself, but before Zebra installs its logs, so those
/// warnings are lost.
pub(super) fn sampler_env_problem(
    sampler: Option<&str>,
    arg: Option<&str>,
) -> Option<&'static str> {
    const RATIO_SAMPLERS: [&str; 2] = ["traceidratio", "parentbased_traceidratio"];
    const OTHER_SAMPLERS: [&str; 4] = [
        "always_on",
        "always_off",
        "parentbased_always_on",
        "parentbased_always_off",
    ];

    match sampler {
        None if arg.is_some() => Some(
            "OTEL_TRACES_SAMPLER_ARG applies only when OTEL_TRACES_SAMPLER is set, \
             so every trace is sampled",
        ),
        None => None,
        Some(sampler) if RATIO_SAMPLERS.contains(&sampler) => {
            match arg.and_then(|arg| arg.parse::<f64>().ok()) {
                None => Some(
                    "OTEL_TRACES_SAMPLER_ARG is missing or not a number, \
                     so every trace is sampled",
                ),
                Some(ratio) if !(0.0..=1.0).contains(&ratio) => {
                    Some("OTEL_TRACES_SAMPLER_ARG is a ratio from 0.0 to 1.0, not a percentage")
                }
                Some(_) => None,
            }
        }
        Some(sampler) if OTHER_SAMPLERS.contains(&sampler) => None,
        Some(_) => Some(
            "OTEL_TRACES_SAMPLER is not a sampler this OpenTelemetry SDK supports, \
             so every trace is sampled unless its remote parent was not",
        ),
    }
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
    fn sampler_env_problems() {
        let cases = [
            (None, None, false),
            (None, Some("10"), true),
            (Some("always_off"), None, false),
            (Some("parentbased_always_on"), Some("0.5"), false),
            (Some("traceidratio"), Some("0.1"), false),
            (Some("traceidratio"), Some("1"), false),
            (Some("parentbased_traceidratio"), Some("0"), false),
            (Some("traceidratio"), None, true),
            (Some("traceidratio"), Some("10%"), true),
            (Some("traceidratio"), Some("10"), true),
            (Some("ratio"), Some("0.1"), true),
        ];

        for (sampler, arg, has_problem) in cases {
            assert_eq!(
                sampler_env_problem(sampler, arg).is_some(),
                has_problem,
                "OTEL_TRACES_SAMPLER={sampler:?} OTEL_TRACES_SAMPLER_ARG={arg:?}",
            );
        }
    }

    /// Changes the process environment, which only this test reads. Nextest runs each test in
    /// its own process.
    #[test]
    fn sampler_comes_from_env_unless_percent_is_configured() {
        let with_env = |sampler: &str, arg: &str, sample_percent| {
            std::env::set_var("OTEL_TRACES_SAMPLER", sampler);
            std::env::set_var("OTEL_TRACES_SAMPLER_ARG", arg);
            sampler_description(sample_percent)
        };

        assert_eq!(with_env("always_off", "", None), "AlwaysOff");
        assert_eq!(
            with_env("traceidratio", "0.25", None),
            "TraceIdRatioBased(0.25)"
        );
        assert_eq!(
            with_env("parentbased_traceidratio", "0", None),
            "ParentBased(TraceIdRatioBased(0.0))"
        );
        assert_eq!(
            with_env("traceidratio", "0.25", Some(10)),
            "TraceIdRatioBased(0.1)"
        );
        assert_eq!(with_env("always_on", "", Some(0)), "AlwaysOff");

        std::env::remove_var("OTEL_TRACES_SAMPLER");
        std::env::remove_var("OTEL_TRACES_SAMPLER_ARG");
        assert_eq!(sampler_description(None), "ParentBased(AlwaysOn)");
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
