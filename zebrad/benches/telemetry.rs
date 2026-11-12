//! Costs of recording metrics and creating spans, with and without telemetry backends installed.
//!
//! Run with `cargo bench -p zebrad --features "sentry prometheus" --bench telemetry`.

// The criterion macros generate undocumented public functions.
#![allow(missing_docs)]

use std::{hint::black_box, sync::Arc};

use criterion::{criterion_group, criterion_main, Criterion};
use metrics::{Counter, Gauge, Histogram, Recorder};
use metrics_exporter_prometheus::PrometheusBuilder;
use metrics_util::layers::FanoutBuilder;
use sentry::{test::TestTransport, ClientOptions, Hub};
use tracing_subscriber::{layer::SubscriberExt, Registry};

/// The recorders that metric operations run against.
enum Backend {
    /// No recorder is installed, so every call is a no-op.
    None,
    /// One Prometheus recorder with no HTTP listener.
    Prometheus,
    /// Two Prometheus recorders behind a fanout, standing in for two backends.
    Fanout,
}

impl Backend {
    const ALL: [Backend; 3] = [Backend::None, Backend::Prometheus, Backend::Fanout];

    fn name(&self) -> &'static str {
        match self {
            Backend::None => "none",
            Backend::Prometheus => "prometheus",
            Backend::Fanout => "fanout_of_two",
        }
    }

    /// Runs `f` with this backend's recorder installed for the current thread.
    fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        match self {
            Backend::None => f(),
            Backend::Prometheus => {
                let recorder = PrometheusBuilder::new().build_recorder();
                metrics::with_local_recorder(&recorder, f)
            }
            Backend::Fanout => {
                let recorder = FanoutBuilder::default()
                    .add_recorder(PrometheusBuilder::new().build_recorder())
                    .add_recorder(PrometheusBuilder::new().build_recorder())
                    .build();
                metrics::with_local_recorder(&recorder as &dyn Recorder, f)
            }
        }
    }
}

fn metrics_benches(c: &mut Criterion) {
    // An owned label value, like call sites that format a label at runtime.
    let label = String::from("In::Req::Peers");

    for backend in Backend::ALL {
        let mut group = c.benchmark_group(format!("metrics/{}", backend.name()));

        backend.run(|| {
            group.bench_function("counter/fixed_name", |b| {
                b.iter(|| metrics::counter!("bench.counter").increment(black_box(1)))
            });
            group.bench_function("counter/dynamic_label", |b| {
                b.iter(|| {
                    metrics::counter!("bench.counter.labeled", "command" => label.clone())
                        .increment(black_box(1))
                })
            });
            let counter: Counter = metrics::counter!("bench.counter.cached");
            group.bench_function("counter/cached_handle", |b| {
                b.iter(|| counter.increment(black_box(1)))
            });

            group.bench_function("gauge/fixed_name", |b| {
                b.iter(|| metrics::gauge!("bench.gauge").set(black_box(1.0)))
            });
            group.bench_function("gauge/dynamic_label", |b| {
                b.iter(|| {
                    metrics::gauge!("bench.gauge.labeled", "command" => label.clone())
                        .set(black_box(1.0))
                })
            });
            let gauge: Gauge = metrics::gauge!("bench.gauge.cached");
            group.bench_function("gauge/cached_handle", |b| {
                b.iter(|| gauge.set(black_box(1.0)))
            });

            group.bench_function("histogram/fixed_name", |b| {
                b.iter(|| metrics::histogram!("bench.histogram").record(black_box(1.0)))
            });
            group.bench_function("histogram/dynamic_label", |b| {
                b.iter(|| {
                    metrics::histogram!("bench.histogram.labeled", "command" => label.clone())
                        .record(black_box(1.0))
                })
            });
            let histogram: Histogram = metrics::histogram!("bench.histogram.cached");
            group.bench_function("histogram/cached_handle", |b| {
                b.iter(|| histogram.record(black_box(1.0)))
            });
        });

        group.finish();
    }
}

/// Returns a hub bound to an enabled client whose events go to an in-memory transport.
fn enabled_hub() -> Arc<Hub> {
    let mut options = ClientOptions::default();
    options.dsn = Some(
        "https://public@example.com/1"
            .parse()
            .expect("valid test DSN"),
    );
    options.transport = Some(Arc::new(TestTransport::new()));
    let client = Arc::new(sentry::Client::from(options));

    Arc::new(Hub::new(Some(client), Arc::default()))
}

fn create_enter_exit() {
    let _span = black_box(tracing::info_span!("bench_span", peer = "127.0.0.1:8233")).entered();
}

fn span_benches(c: &mut Criterion) {
    let mut group = c.benchmark_group("spans");

    group.bench_function("no_subscriber", |b| b.iter(create_enter_exit));

    tracing::subscriber::with_default(Registry::default(), || {
        group.bench_function("registry_only", |b| b.iter(create_enter_exit));
    });

    Hub::run(enabled_hub(), || {
        let subscriber = Registry::default().with(sentry::integrations::tracing::layer());
        tracing::subscriber::with_default(subscriber, || {
            group.bench_function("sentry_layer_default_span_filter", |b| {
                b.iter(create_enter_exit)
            });
        });

        let subscriber =
            Registry::default().with(sentry::integrations::tracing::layer().span_filter(|_| false));
        tracing::subscriber::with_default(subscriber, || {
            group.bench_function("sentry_layer_span_filter_false", |b| {
                b.iter(create_enter_exit)
            });
        });
    });

    group.finish();
}

criterion_group!(benches, metrics_benches, span_benches);
criterion_main!(benches);
