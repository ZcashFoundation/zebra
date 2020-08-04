//! An HTTP endpoint for dynamically setting tracing filters.

use crate::{components::tokio::TokioComponent, config::TracingSection, prelude::*};
use abscissa_core::{trace::Tracing, Component, FrameworkError};
use color_eyre::eyre::Report;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use once_cell::sync::Lazy;
use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tracing_subscriber::EnvFilter;

static FLAMEGRAPH_ENV: &str = "ZEBRAD_FLAMEGRAPH";

/// Abscissa component which runs a tracing filter endpoint.
#[derive(Debug, Component)]
#[component(inject = "init_tokio(zebrad::components::tokio::TokioComponent)")]
pub struct TracingEndpoint {}

async fn read_filter(req: Request<Body>) -> Result<String, String> {
    std::str::from_utf8(
        &hyper::body::to_bytes(req.into_body())
            .await
            .map_err(|_| "Error reading body".to_owned())?,
    )
    .map(|s| s.to_owned())
    .map_err(|_| "Filter must be UTF-8".to_owned())
}

impl TracingEndpoint {
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

    /// Open the tracing endpoint.
    ///
    /// We can't implement `after_config`, because we use `derive(Component)`.
    /// And the ownership rules might make it hard to access the TokioComponent
    /// from `after_config`.
    pub fn open_endpoint(&self, tracing_config: &TracingSection, tokio_component: &TokioComponent) {
        info!("Initializing tracing endpoint");

        let service =
            make_service_fn(|_| async { Ok::<_, hyper::Error>(service_fn(request_handler)) });

        let addr = tracing_config.endpoint_addr;

        tokio_component
            .rt
            .as_ref()
            .expect("runtime should not be taken")
            .spawn(async move {
                // try_bind uses the tokio runtime, so we
                // need to construct it inside the task.
                let server = match Server::try_bind(&addr) {
                    Ok(s) => s,
                    Err(e) => {
                        error!("Could not open tracing endpoint listener");
                        error!("Error: {}", e);
                        return;
                    }
                }
                .serve(service);

                if let Err(e) = server.await {
                    error!("Server error: {}", e);
                }
            });
    }
}

#[instrument]
async fn request_handler(req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    use hyper::{Method, StatusCode};

    let rsp = match (req.method(), req.uri().path()) {
        (&Method::GET, "/") => Response::new(Body::from(
            r#"
This HTTP endpoint allows dynamic control of the filter applied to
tracing events.

To get the current filter, GET /filter:

    curl -X GET localhost:3000/filter

To set the filter, POST the new filter string to /filter:

    curl -X POST localhost:3000/filter -d "zebrad=trace"
"#,
        )),
        (&Method::GET, "/filter") => Response::builder()
            .status(StatusCode::OK)
            .body(Body::from(
                app_reader()
                    .state()
                    .components
                    .get_downcast_ref::<abscissa_core::trace::Tracing>()
                    .expect("Tracing component should be available")
                    .filter(),
            ))
            .expect("response with known status code cannot fail"),
        (&Method::POST, "/filter") => match read_filter(req).await {
            Ok(filter) => {
                app_writer()
                    .state_mut()
                    .components
                    .get_downcast_mut::<abscissa_core::trace::Tracing>()
                    .expect("Tracing component should be available")
                    .reload_filter(filter);

                Response::new(Body::from(""))
            }
            Err(e) => Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Body::from(e))
                .expect("response with known status code cannot fail"),
        },
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::from(""))
            .expect("response with known status cannot fail"),
    };
    Ok(rsp)
}

/// Global handle to flame guard for signal handler termination
///
/// This needs to be independent of the owned flame_guard below to avoid
/// contention on the AppCell's lock
pub(crate) static FLAME_GUARD: Lazy<Mutex<Option<FlameGrapher>>> = Lazy::new(|| Mutex::new(None));

#[derive(Clone)]
pub(crate) struct FlameGrapher {
    guard: Arc<tracing_flame::FlushGuard<BufWriter<File>>>,
    path: PathBuf,
}

impl FlameGrapher {
    fn make_flamegraph(&self) -> Result<(), Report> {
        self.guard.flush()?;
        let out_path = self.path.with_extension("svg");
        let inf = File::open(&self.path)?;
        let reader = BufReader::new(inf);

        let out = File::create(out_path)?;
        let writer = BufWriter::new(out);

        let mut opts = inferno::flamegraph::Options::default();
        info!("writing flamegraph to disk...");
        inferno::flamegraph::from_reader(&mut opts, reader, writer)?;

        Ok(())
    }
}

impl Drop for FlameGrapher {
    fn drop(&mut self) {
        match self.make_flamegraph() {
            Ok(()) => {}
            Err(report) => {
                warn!(
                    "Error while constructing flamegraph during shutdown: {:?}",
                    report
                );
            }
        }
    }
}

pub(crate) fn init(level: EnvFilter) -> (Tracing, Option<FlameGrapher>) {
    use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

    // Construct a tracing subscriber with the supplied filter and enable reloading.
    let builder = tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(level)
        .with_filter_reloading();
    let filter_handle = builder.reload_handle();
    let subscriber = builder.finish().with(tracing_error::ErrorLayer::default());

    let guard = if let Ok(flamegraph_path) = std::env::var(FLAMEGRAPH_ENV) {
        let flamegraph_path = Path::new(&flamegraph_path).with_extension("folded");
        let (flame_layer, guard) = tracing_flame::FlameLayer::with_file(&flamegraph_path).unwrap();
        let flame_layer = flame_layer
            .with_empty_samples(false)
            .with_threads_collapsed(true);
        subscriber.with(flame_layer).init();
        Some(FlameGrapher {
            guard: Arc::new(guard),
            path: flamegraph_path,
        })
    } else {
        subscriber.init();
        None
    };

    *FLAME_GUARD.lock().unwrap() = guard.clone();
    (filter_handle.into(), guard)
}

pub(crate) fn cleanup_tracing() {
    if let Some(flamegrapher) = FLAME_GUARD.lock().unwrap().as_ref() {
        match flamegrapher.make_flamegraph() {
            Ok(()) => {}
            Err(report) => {
                warn!(
                    "Error while constructing flamegraph during shutdown: {:?}",
                    report
                );
            }
        }
    }
}
