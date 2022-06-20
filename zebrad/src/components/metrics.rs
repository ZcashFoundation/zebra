//! An HTTP endpoint for metrics collection.

use abscissa_core::{Component, FrameworkError};

use crate::config::ZebradConfig;

/// Abscissa component which runs a metrics endpoint.
#[derive(Debug, Component)]
pub struct MetricsEndpoint {}

impl MetricsEndpoint {
    /// Create the component.
    #[cfg(feature = "prometheus")]
    pub fn new(config: &ZebradConfig) -> Result<Self, FrameworkError> {
        if let Some(addr) = config.metrics.endpoint_addr {
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
                    metrics::increment_counter!(
                        format!("{}.build.info", env!("CARGO_PKG_NAME")),
                        "version" => env!("CARGO_PKG_VERSION")
                    );
                }
                Err(e) => panic!(
                    "Opening metrics endpoint listener {:?} failed: {:?}. \
                     Hint: Check if another zebrad or zcashd process is running. \
                     Try changing the metrics endpoint_addr in the Zebra config.",
                    addr, e,
                ),
            }
        }

        Ok(Self {})
    }

    /// Create the component.
    #[cfg(not(feature = "prometheus"))]
    pub fn new(config: &ZebradConfig) -> Result<Self, FrameworkError> {
        if let Some(addr) = config.metrics.endpoint_addr {
            warn!(
                ?addr,
                "unable to activate configured metrics endpoint: \
                 enable the 'prometheus' feature when compiling zebrad",
            );
        }

        Ok(Self {})
    }
}
