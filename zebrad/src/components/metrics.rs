//! An HTTP endpoint for metrics collection.

#![allow(non_local_definitions)]

use std::net::SocketAddr;

use abscissa_core::{Component, FrameworkError, FrameworkErrorKind};
use metrics::Recorder;
use serde::{Deserialize, Serialize};

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

        if let Some(addr) = config.endpoint_addr {
            Self::open_endpoint(addr, sentry)?;
        } else if let Some(sentry) = sentry {
            install(sentry)?;
        }

        Ok(Self {})
    }

    /// Opens the Prometheus endpoint, also recording metrics with `other` if it is provided.
    #[cfg(feature = "prometheus")]
    fn open_endpoint<R>(addr: SocketAddr, other: Option<R>) -> Result<(), FrameworkError>
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

        Ok(())
    }

    /// Warns that the endpoint can't be opened, and records metrics with `other` if it is
    /// provided.
    #[cfg(not(feature = "prometheus"))]
    fn open_endpoint<R>(addr: SocketAddr, other: Option<R>) -> Result<(), FrameworkError>
    where
        R: Recorder + Send + Sync + 'static,
    {
        warn!(
            ?addr,
            "unable to activate configured metrics endpoint: \
             enable the 'prometheus' feature when compiling zebrad",
        );

        other.map_or(Ok(()), install)
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
