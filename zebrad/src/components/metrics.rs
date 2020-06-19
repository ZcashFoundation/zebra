//! An HTTP endpoint for metrics collection.

use metrics_runtime::{exporters::HttpExporter, observers::PrometheusBuilder, Receiver};

use abscissa_core::{Component, FrameworkError};

use crate::components::tokio::TokioComponent;

/// Abscissa component which runs a metrics endpoint.
#[derive(Debug, Component)]
#[component(inject = "init_tokio(zebrad::components::tokio::TokioComponent)")]
pub struct MetricsEndpoint {}

impl MetricsEndpoint {
    /// Create the component.
    pub fn new() -> Result<Self, FrameworkError> {
        Ok(Self {})
    }

    /// Do setup after receiving a tokio runtime.
    pub fn init_tokio(&mut self, tokio_component: &TokioComponent) -> Result<(), FrameworkError> {
        info!("Initializing metrics endpoint");

        // XXX load metrics addr from config
        let addr = "0.0.0.0:9999"
            .parse()
            .expect("Hardcoded address should be parseable");

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
