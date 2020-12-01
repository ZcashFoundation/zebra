//! An HTTP endpoint for dynamically setting tracing filters.

use std::net::SocketAddr;

use abscissa_core::{Component, FrameworkError};
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};

use crate::{components::tokio::TokioComponent, config::ZebradConfig, prelude::*};

use super::Tracing;

/// Abscissa component which runs a tracing filter endpoint.
#[derive(Debug, Component)]
#[component(inject = "init_tokio(zebrad::components::tokio::TokioComponent)")]
pub struct TracingEndpoint {
    addr: Option<SocketAddr>,
}

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
    pub fn new(config: &ZebradConfig) -> Result<Self, FrameworkError> {
        Ok(Self {
            addr: config.tracing.endpoint_addr,
        })
    }

    pub fn init_tokio(&mut self, tokio_component: &TokioComponent) -> Result<(), FrameworkError> {
        let addr = if let Some(addr) = self.addr {
            addr
        } else {
            return Ok(());
        };
        info!("Initializing tracing endpoint at {}", addr);

        let service =
            make_service_fn(|_| async { Ok::<_, hyper::Error>(service_fn(request_handler)) });

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

        Ok(())
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
                    .get_downcast_ref::<Tracing>()
                    .expect("Tracing component should be available")
                    .filter(),
            ))
            .expect("response with known status code cannot fail"),
        (&Method::POST, "/filter") => match read_filter(req).await {
            Ok(filter) => {
                app_reader()
                    .state()
                    .components
                    .get_downcast_ref::<Tracing>()
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
