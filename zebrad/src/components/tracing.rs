//! An HTTP endpoint for dynamically setting tracing filters.

use crate::components::tokio::TokioComponent;

use abscissa_core::{err, Component, FrameworkError, FrameworkErrorKind};

use hyper::{Body, Request, Response, Server, Version};
use hyper::service::{make_service_fn, service_fn};

// XXX upstream API will be reworked soon: fmt is merging with subscriber
// https://github.com/tokio-rs/tracing/pull/311

use tracing_fmt::FmtSubscriber;
use tracing_log::LogTracer;
use tracing_subscriber::{
    filter::Filter,
    layer::SubscriberExt,
    reload::{Handle, Layer},
};

// Horrible compound type
type OurSubscriber = FmtSubscriber<
    tracing_fmt::format::NewRecorder,
    tracing_fmt::format::Format<tracing_fmt::format::Full>,
    tracing_fmt::filter::NoFilter,
>;

/// Abscissa component which runs a tracing filter endpoint.
#[derive(Component)]
#[component(inject = "init_tokio(zebrad::components::tokio::TokioComponent)")]
// XXX ideally this would be TracingEndpoint<S: Subscriber>
// but this doesn't seem to play well with derive(Component)
pub struct TracingEndpoint {
    filter_handle: Handle<Filter, OurSubscriber>,
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

        // Create a dynamic filter for log events and retain its handle.
        // XXX pull from config
        let (filter, filter_handle) = Layer::new(Filter::new("info"));

        // Apply that filter to a tracing subscriber.
        let subscriber = tracing_fmt::FmtSubscriber::builder()
            .with_ansi(true)
            .with_filter(tracing_fmt::filter::none())
            .finish()
            .with(filter); // from SubscriberExt

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
