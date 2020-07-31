//! An HTTP endpoint for metrics collection.

use crate::{components::tokio::TokioComponent, config::MetricsSection};

use abscissa_core::{Component, FrameworkError};

use metrics_runtime::{exporters::HttpExporter, observers::PrometheusBuilder, Receiver};

/// Abscissa component which runs a metrics endpoint.
#[derive(Debug, Component)]
#[component(inject = "init_tokio(zebrad::components::tokio::TokioComponent)")]
pub struct MetricsEndpoint {}

impl MetricsEndpoint {
    /// Create the component.
    pub fn new() -> Result<Self, FrameworkError> {
        Ok(Self {})
    }

    /// Tokio endpoint dependency stub.
    ///
    /// We can't open the endpoint here, because the config has not been loaded.
    pub fn init_tokio(&mut self, _tokio_component: &TokioComponent) -> Result<(), FrameworkError> {
        Ok(())
    }

    /// Open the metrics endpoint.
    ///
    /// We can't implement `after_config`, because we use `derive(Component)`.
    /// And the ownership rules might make it hard to access the TokioComponent
    /// from `after_config`.
    pub fn open_endpoint(&self, metrics_config: &MetricsSection, tokio_component: &TokioComponent) {
        info!("Initializing metrics endpoint");

        let addr = metrics_config.endpoint_addr;

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
    }
}
