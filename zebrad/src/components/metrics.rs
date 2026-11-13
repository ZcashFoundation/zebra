//! An HTTP endpoint for metrics collection.

#![allow(non_local_definitions)]

use std::{net::SocketAddr, time::Duration};

use abscissa_core::{Component, FrameworkError, FrameworkErrorKind};
use metrics::Recorder;
use serde::{Deserialize, Serialize};

pub mod catalog;

// `metrics-process` describes fewer metrics on Windows, so the catalog only matches elsewhere.
#[cfg(all(test, not(target_os = "windows")))]
mod tests;

/// How often the process metrics are sampled.
const PROCESS_METRICS_INTERVAL: Duration = Duration::from_secs(15);

/// The histograms that Prometheus exports as histograms with these bucket upper bounds, in
/// seconds. Every other histogram is exported as a summary with quantiles.
///
/// Adding a histogram here removes its `quantile` series, which breaks queries that use them.
pub const HISTOGRAM_BUCKETS: &[(&str, &[f64])] = &[
    (
        "rpc.request.duration_seconds",
        &[
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0,
        ],
    ),
    (
        "zcash.net.peer.handshake.duration_seconds",
        &[
            0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.0, 3.0, 5.0, 10.0, 30.0,
        ],
    ),
];

/// Abscissa component which runs a metrics endpoint.
#[derive(Debug, Component)]
pub struct MetricsEndpoint {}

impl MetricsEndpoint {
    /// Create the component.
    pub fn new(config: &Config) -> Result<Self, FrameworkError> {
        #[cfg(feature = "sentry")]
        let sentry = crate::sentry::metrics_recorder();
        #[cfg(not(feature = "sentry"))]
        let sentry = None::<metrics::NoopRecorder>;

        let recorder_installed = if let Some(addr) = config.endpoint_addr {
            Self::open_endpoint(addr, sentry)?
        } else if let Some(sentry) = sentry {
            install(sentry)?;
            true
        } else {
            false
        };

        if recorder_installed {
            describe_all();
            zebra_consensus::metrics::register_failure_counters();
            spawn_process_metrics()?;
        }

        Ok(Self {})
    }

    /// Opens the Prometheus endpoint, also recording metrics with `other` if it is provided.
    ///
    /// Returns whether a global recorder was installed.
    #[cfg(feature = "prometheus")]
    fn open_endpoint<R>(addr: SocketAddr, other: Option<R>) -> Result<bool, FrameworkError>
    where
        R: Recorder + Send + Sync + 'static,
    {
        info!("Trying to open metrics endpoint at {}...", addr);

        let builder = prometheus_builder()
            .map_err(|e| FrameworkErrorKind::ComponentError.context(e))?
            .with_http_listener(addr);

        let endpoint_result = match other {
            Some(other) => install_alongside(builder, other),
            None => builder.install().map_err(Into::into),
        };

        match endpoint_result {
            Ok(()) => {
                // Expose binary metadata to metrics, using a single time series with
                // value 1:
                //     https://www.robustperception.io/exposing-the-software-version-to-prometheus
                metrics::counter!(
                    format!("{}.build.info", env!("CARGO_PKG_NAME")),
                    "version" => env!("CARGO_PKG_VERSION")
                )
                .increment(1);

                info!("Opened metrics endpoint at {}", addr);
            }
            Err(e) => panic!(
                "Opening metrics endpoint listener {addr:?} failed: {e:?}. \
                 Hint: Check if another zebrad or zcashd process is running. \
                 Try changing the metrics endpoint_addr in the Zebra config.",
            ),
        }

        Ok(true)
    }

    /// Warns that the endpoint can't be opened, and records metrics with `other` if it is
    /// provided.
    ///
    /// Returns whether a global recorder was installed.
    #[cfg(not(feature = "prometheus"))]
    fn open_endpoint<R>(addr: SocketAddr, other: Option<R>) -> Result<bool, FrameworkError>
    where
        R: Recorder + Send + Sync + 'static,
    {
        warn!(
            ?addr,
            "unable to activate configured metrics endpoint: \
             enable the 'prometheus' feature when compiling zebrad",
        );

        other.map_or(Ok(false), |other| install(other).map(|()| true))
    }
}

/// Describes every metric Zebra records to the current recorder.
///
/// Call this right after installing the global recorder: descriptions sent while no recorder is
/// installed are lost.
pub fn describe_all() {
    zebra_consensus::metrics::describe();
    zebra_network::metrics::describe();
    zebra_state::metrics::describe();
    zebra_rpc::metrics::describe();
    catalog::describe();
    metrics_process::Collector::default().describe();
}

/// Records the process metrics through the global recorder, which must already be installed.
///
/// Samples once immediately, then every [`PROCESS_METRICS_INTERVAL`] on a dedicated thread, so
/// the readings neither depend on a backend nor run on a Tokio worker.
fn spawn_process_metrics() -> Result<(), FrameworkError> {
    let collector = metrics_process::Collector::default();
    collector.collect();

    std::thread::Builder::new()
        .name("process-metrics".to_owned())
        .spawn(move || loop {
            std::thread::sleep(PROCESS_METRICS_INTERVAL);
            collector.collect();
        })
        .map(drop)
        .map_err(|e| FrameworkErrorKind::ComponentError.context(e).into())
}

/// Installs `recorder` as the global metrics recorder.
fn install<R>(recorder: R) -> Result<(), FrameworkError>
where
    R: Recorder + Send + Sync + 'static,
{
    metrics::set_global_recorder(recorder)
        .map_err(|e| FrameworkErrorKind::ComponentError.context(e).into())
}

/// Returns the Prometheus exporter's configuration, before its endpoint is added.
#[cfg(feature = "prometheus")]
fn prometheus_builder(
) -> Result<metrics_exporter_prometheus::PrometheusBuilder, metrics_exporter_prometheus::BuildError>
{
    HISTOGRAM_BUCKETS.iter().try_fold(
        metrics_exporter_prometheus::PrometheusBuilder::new(),
        |builder, (name, buckets)| {
            builder.set_buckets_for_metric(
                metrics_exporter_prometheus::Matcher::Full((*name).to_owned()),
                buckets,
            )
        },
    )
}

/// Installs the Prometheus recorder from `builder` and `other` as one recorder.
///
/// Like [`PrometheusBuilder::install`] outside a Tokio runtime, which is how Zebra calls it, this
/// serves the endpoint from its own thread.
///
/// [`PrometheusBuilder::install`]: metrics_exporter_prometheus::PrometheusBuilder::install
#[cfg(feature = "prometheus")]
fn install_alongside<R>(
    builder: metrics_exporter_prometheus::PrometheusBuilder,
    other: R,
) -> Result<(), crate::BoxError>
where
    R: Recorder + Send + Sync + 'static,
{
    use metrics_exporter_prometheus::BuildError;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| BuildError::FailedToCreateRuntime(e.to_string()))?;

    let (prometheus, exporter) = {
        let _guard = runtime.enter();
        builder.build()?
    };

    std::thread::Builder::new()
        .name("metrics-exporter-prometheus-http".to_owned())
        .spawn(move || runtime.block_on(exporter))
        .map_err(|e| BuildError::FailedToCreateRuntime(e.to_string()))?;

    metrics::set_global_recorder(
        metrics_util::layers::FanoutBuilder::default()
            .add_recorder(prometheus)
            .add_recorder(other)
            .build(),
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Metrics configuration section.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// The address used for the Prometheus metrics endpoint.
    ///
    /// Install Zebra using `cargo install --features=prometheus` to enable this config.
    ///
    /// The endpoint is disabled if this is set to `None`.
    pub endpoint_addr: Option<SocketAddr>,
}

// we like our default configs to be explicit
#[allow(unknown_lints)]
#[allow(clippy::derivable_impls)]
impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint_addr: None,
        }
    }
}
