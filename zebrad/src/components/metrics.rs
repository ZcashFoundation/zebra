//! An HTTP endpoint for metrics collection.

use std::net::SocketAddr;

use abscissa_core::{Component, FrameworkError};
use metrics_runtime::{exporters::HttpExporter, observers::PrometheusBuilder, Receiver};

use crate::{components::tokio::TokioComponent, config::ZebradConfig};

/// Abscissa component which runs a metrics endpoint.
#[derive(Debug, Component)]
#[component(inject = "init_tokio(zebrad::components::tokio::TokioComponent)")]
pub struct MetricsEndpoint {
    addr: Option<SocketAddr>,
}

impl MetricsEndpoint {
    /// Create the component.
    pub fn new(config: &ZebradConfig) -> Result<Self, FrameworkError> {
        Ok(Self {
            addr: config.metrics.endpoint_addr,
        })
    }

    /// Tokio endpoint dependency stub.
    pub fn init_tokio(&mut self, tokio_component: &TokioComponent) -> Result<(), FrameworkError> {
        let addr = if let Some(addr) = self.addr {
            addr
        } else {
            return Ok(());
        };

        info!("Initializing metrics endpoint at {}", addr);

        // XXX do we need to hold on to the receiver?
        let receiver = Receiver::builder()
            .build()
            .expect("Receiver config should be valid");
        // XXX ???? connect this ???
        let _sink = receiver.sink();

        let endpoint = HttpExporter::new(receiver.controller(), PrometheusBuilder::new(), addr);

        tokio_component
            .rt
            .as_ref()
            .expect("runtime should not be taken")
            .spawn(endpoint.async_run());

        metrics::set_boxed_recorder(Box::new(receiver)).expect("XXX FIXME ERROR CONVERSION");

        Ok(())
    }
}
