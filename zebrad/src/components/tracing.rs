//! An HTTP endpoint for dynamically setting tracing filters.

use crate::components::tokio::TokioComponent;

use abscissa_core::{err, Component, FrameworkError, FrameworkErrorKind};

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, Version};

use tracing_log::LogTracer;
use tracing_subscriber::{
    filter::Filter,
    reload::{Handle},
    FmtSubscriber,
};

/// Abscissa component which runs a tracing filter endpoint.
#[derive(Component)]
#[component(inject = "init_tokio(zebrad::components::tokio::TokioComponent)")]
// XXX ideally this would be TracingEndpoint<S: Subscriber>
// but this doesn't seem to play well with derive(Component)
pub struct TracingEndpoint {
    filter_handle: Handle<Filter, FmtSubscriber>,
}

impl ::std::fmt::Debug for TracingEndpoint {
    fn fmt(&self, f: &mut ::std::fmt::Formatter) -> Result<(), ::std::fmt::Error> {
        // Debug is required by Component, can't be derived as a Handle is not Debug
        write!(f, "TracingEndpoint")
    }
}

impl TracingEndpoint {
    /// Create the component.
    pub fn new() -> Result<Self, FrameworkError> {
        // Set the global logger for the log crate to emit tracing events.
        // XXX this is only required if we have a dependency that uses log;
        // currently this is maybe only abscissa itself?
        LogTracer::init().map_err(|e| {
            err!(
                FrameworkErrorKind::ComponentError,
                "could not set log subscriber: {}",
                e
            )
        })?;

        let builder = FmtSubscriber::builder()
            .with_ansi(true)
            // Set the initial filter from the RUST_LOG env variable
            // XXX pull from config file?
            .with_filter(Filter::from_default_env())
            .with_filter_reloading();
        let filter_handle = builder.reload_handle();
        let subscriber = builder.finish();

        // Set that subscriber to be the global tracing subscriber
        tracing::subscriber::set_global_default(subscriber).map_err(|e| {
            err!(
                FrameworkErrorKind::ComponentError,
                "could not set tracing subscriber: {}",
                e
            )
        })?;

        Ok(Self { filter_handle })
    }

    /// Do setup after receiving a tokio runtime.
    pub fn init_tokio(&mut self, tokio_component: &TokioComponent) -> Result<(), FrameworkError> {
        async fn hello_world_handler(req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
            Ok(Response::new(Body::from("Hello World! 👋")))
        }

        let service =
            make_service_fn(|_| async { Ok::<_, hyper::Error>(service_fn(hello_world_handler)) });

        let addr = "127.0.0.1:3000".parse().unwrap();

        let server = Server::bind(&addr).serve(service);

        tokio_component.rt.spawn(async {
            if let Err(e) = server.await {
                error!("Server error: {}", e);
            }
        });

        Ok(())
    }
}
