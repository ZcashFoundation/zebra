//! An HTTP endpoint for metrics collection.

use abscissa_core::{Component, FrameworkError};

use crate::config::ZebradConfig;

/// Abscissa component which runs a metrics endpoint.
#[derive(Debug, Component)]
pub struct MetricsEndpoint {}

impl MetricsEndpoint {
    /// Create the component.
    pub fn new(config: &ZebradConfig) -> Result<Self, FrameworkError> {
        if let Some(addr) = config.metrics.endpoint_addr {
            info!("Initializing metrics endpoint at {}", addr);
            let endpoint_result = metrics_exporter_prometheus::PrometheusBuilder::new()
                .listen_address(addr)
                .install();
            match endpoint_result {
                Ok(endpoint) => endpoint,
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
}
