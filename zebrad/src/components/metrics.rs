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
            metrics_exporter_prometheus::PrometheusBuilder::new()
                .listen_address(addr)
                .install()
                .expect("FIXME ERROR CONVERSION");
        }
        Ok(Self {})
    }
}
