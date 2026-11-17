//! An HTTP endpoint for metrics collection.

#![allow(non_local_definitions)]

use std::net::SocketAddr;

use abscissa_core::{Component, FrameworkError, FrameworkErrorKind};
use metrics::Recorder;
use serde::{Deserialize, Serialize};

mod process_memory;

#[cfg(all(test, feature = "sentry"))]
pub(crate) use process_memory::record;
pub use process_memory::record_process_memory;

/// How often the process metrics are sampled.
#[cfg(not(windows))]
const PROCESS_METRICS_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

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
            spawn_process_metrics();
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

        let builder =
            metrics_exporter_prometheus::PrometheusBuilder::new().with_http_listener(addr);
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

/// Records the process metrics through the global recorder, which must already be installed.
///
/// Samples once immediately, then every [`PROCESS_METRICS_INTERVAL`] on a dedicated thread, so
/// the readings neither depend on a backend nor run on a Tokio worker.
#[cfg(not(windows))]
fn spawn_process_metrics() {
    use metrics::{describe_counter, describe_gauge, Unit};

    describe_counter!(
        "process_cpu_seconds_total",
        Unit::Seconds,
        "Total user and system CPU time spent in seconds."
    );
    for (name, unit, description) in [
        (
            "process_open_fds",
            Unit::Count,
            "Number of open file descriptors.",
        ),
        (
            "process_max_fds",
            Unit::Count,
            "Maximum number of open file descriptors.",
        ),
        (
            "process_virtual_memory_bytes",
            Unit::Bytes,
            "Virtual memory size in bytes.",
        ),
        (
            "process_virtual_memory_max_bytes",
            Unit::Bytes,
            "Maximum amount of virtual memory available in bytes.",
        ),
        (
            "process_resident_memory_bytes",
            Unit::Bytes,
            "Resident memory size in bytes.",
        ),
        (
            "process_start_time_seconds",
            Unit::Seconds,
            "Start time of the process since unix epoch in seconds.",
        ),
        (
            "process_threads",
            Unit::Count,
            "Number of OS threads in the process.",
        ),
    ] {
        describe_gauge!(name, unit, description);
    }

    record_process_metrics();

    let sampler = std::thread::Builder::new()
        .name("process-metrics".to_owned())
        .spawn(|| loop {
            std::thread::sleep(PROCESS_METRICS_INTERVAL);
            record_process_metrics();
        });
    if let Err(error) = sampler {
        warn!(?error, "unable to start periodic process metrics");
    }
}

/// Process metrics aren't recorded on Windows, see the `metrics-process` dependency.
#[cfg(windows)]
fn spawn_process_metrics() {}

/// Records one sample of the process metrics.
#[cfg(not(windows))]
fn record_process_metrics() {
    let sample = metrics_process::collector::collect();

    if let Some(seconds) = sample.cpu_seconds_total {
        // `metrics` counters only hold whole numbers, so this drops the fraction of a second.
        metrics::counter!("process_cpu_seconds_total").absolute(seconds as u64);
    }

    // `metrics-process` reports an unlimited maximum as 0, which isn't a usable maximum.
    let limited = |max: Option<u64>| max.filter(|&max| max != 0);

    for (name, value) in [
        ("process_open_fds", sample.open_fds),
        ("process_max_fds", limited(sample.max_fds)),
        ("process_virtual_memory_bytes", sample.virtual_memory_bytes),
        (
            "process_virtual_memory_max_bytes",
            limited(sample.virtual_memory_max_bytes),
        ),
        (
            "process_resident_memory_bytes",
            sample.resident_memory_bytes,
        ),
        ("process_start_time_seconds", sample.start_time_seconds),
        ("process_threads", sample.threads),
    ] {
        if let Some(value) = value {
            // Process sizes, counts, and times stay far below 2^53, so `f64` holds them exactly.
            metrics::gauge!(name).set(value as f64);
        }
    }
}

/// Installs `recorder` as the global metrics recorder.
fn install<R>(recorder: R) -> Result<(), FrameworkError>
where
    R: Recorder + Send + Sync + 'static,
{
    metrics::set_global_recorder(recorder)
        .map_err(|e| FrameworkErrorKind::ComponentError.context(e).into())
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
