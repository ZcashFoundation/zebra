//! An HTTP endpoint for dynamically setting tracing filters.

use crate::components::tokio::TokioComponent;

use abscissa_core::{err, Component, FrameworkError, FrameworkErrorKind};

use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};

use tracing::Subscriber;
use tracing_log::LogTracer;
use tracing_subscriber::{filter::Filter, reload::Handle, FmtSubscriber};

/// Abscissa component which runs a tracing filter endpoint.
#[derive(Component)]
#[component(inject = "init_tokio(zebrad::components::tokio::TokioComponent)")]
// XXX ideally this would be TracingEndpoint<S: Subscriber>
// but this doesn't seem to play well with derive(Component)
pub struct TracingEndpoint {
    filter_handle: Handle<Filter, tracing_subscriber::fmt::Formatter>,
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
        info!("Initializing tracing endpoint");

        // Clone the filter handle so it can be moved into make_service_fn closure
        let handle = self.filter_handle.clone();
        let service = make_service_fn(move |_| {
            // Clone again to move into the service_fn closure
            let handle = handle.clone();
            async move {
                Ok::<_, hyper::Error>(service_fn(move |req| filter_handler(handle.clone(), req)))
            }
        });

        // XXX load tracing addr from config
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

fn reload_filter_from_chunk<S: Subscriber>(
    handle: Handle<Filter, S>,
    chunk: hyper::Chunk,
) -> Result<(), String> {
    let bytes = chunk.into_bytes();
    let body = std::str::from_utf8(bytes.as_ref()).map_err(|e| format!("{}", e))?;
    trace!(request.body = ?body);
    let filter = body.parse::<Filter>().map_err(|e| format!("{}", e))?;
    handle.reload(filter).map_err(|e| format!("{}", e))
}

async fn filter_handler<S: Subscriber>(
    handle: Handle<Filter, S>,
    req: Request<Body>,
) -> Result<Response<Body>, hyper::Error> {
    use futures_util::TryStreamExt;
    use hyper::{Method, StatusCode};

    // We can't use #[instrument] because Handle<_,_> is not Debug,
    // so we create a span manually.
    let handler_span =
        info_span!("filter_handler", method = ?req.method(), path = ?req.uri().path());
    let _enter = handler_span.enter(); // dropping _enter closes the span

    let rsp = match (req.method(), req.uri().path()) {
        (&Method::GET, "/") => Response::new(Body::from(
            r#"
This HTTP endpoint allows dynamic control of the filter applied to
tracing events.  To set the filter, POST it to /filter:

curl -X POST localhost:3000/filter -d "zebrad=trace"
"#,
        )),
        (&Method::POST, "/filter") => {
            // Combine all HTTP request chunks into one
            let whole_chunk = req.into_body().try_concat().await?;
            match reload_filter_from_chunk(handle, whole_chunk) {
                Err(e) => Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(Body::from(e))
                    .expect("response with known status code cannot fail"),
                Ok(()) => Response::new(Body::from("")),
            }
        }
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from(""))
            .expect("response with known status cannot fail"),
    };
    Ok(rsp)
}
