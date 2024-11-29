//! An HTTP endpoint for metrics collection.

#![allow(non_local_definitions)]

use std::net::SocketAddr;

use abscissa_core::{Component, FrameworkError};
use serde::{Deserialize, Serialize};

/// Abscissa component which runs a metrics endpoint.
#[derive(Debug, Component)]
pub struct MetricsEndpoint {}

impl MetricsEndpoint {
    /// Create the component.
    #[cfg(feature = "prometheus")]
    pub fn new(config: &Config) -> Result<Self, FrameworkError> {
        if let Some(addr) = config.endpoint_addr {
            info!("Trying to open metrics endpoint at {}...", addr);

            let endpoint_result = metrics_exporter_prometheus::PrometheusBuilder::new()
                .with_http_listener(addr)
                .install();

            match endpoint_result {
                Ok(()) => {
                    info!("Opened metrics endpoint at {}", addr);

                    // Expose binary metadata to metrics, using a single time series with
                    // value 1:
                    //     https://www.robustperception.io/exposing-the-software-version-to-prometheus
                    metrics::counter!(
                        format!("{}.build.info", env!("CARGO_PKG_NAME")),
                        "version" => env!("CARGO_PKG_VERSION")
                    )
                    .increment(1);
                }
                Err(e) => panic!(
                    "Opening metrics endpoint listener {addr:?} failed: {e:?}. \
                     Hint: Check if another zebrad or zcashd process is running. \
                     Try changing the metrics endpoint_addr in the Zebra config.",
                ),
            }
        }

        Ok(Self {})
    }

    /// Create the component.
    #[cfg(not(feature = "prometheus"))]
    pub fn new(config: &Config) -> Result<Self, FrameworkError> {
        if let Some(addr) = config.endpoint_addr {
            warn!(
                ?addr,
                "unable to activate configured metrics endpoint: \
                 enable the 'prometheus' feature when compiling zebrad",
            );
        }

        Ok(Self {})
    }
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
