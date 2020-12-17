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
            let check_endpoint = metrics_exporter_prometheus::PrometheusBuilder::new()
                .listen_address(addr)
                .install();
            match check_endpoint {
                Ok(endpoint) => endpoint,
                Err(_) => panic!(
                    "{} {} {}",
                    "Port for metrics endpoint already in use by another process:",
                    addr,
                    "- You can change the metrics endpoint in the config."
                ),
            }
        }
        Ok(Self {})
    }
}
