//! An HTTP endpoint for dynamically setting tracing filters.

use std::net::SocketAddr;

use abscissa_core::{Component, FrameworkError};

use crate::config::ZebradConfig;

#[cfg(feature = "filter-reload")]
use hyper::{Body, Request, Response};

#[cfg(feature = "filter-reload")]
use crate::{components::tokio::TokioComponent, prelude::*};

/// Abscissa component which runs a tracing filter endpoint.
#[derive(Debug, Component)]
#[cfg_attr(
    feature = "filter-reload",
    component(inject = "init_tokio(zebrad::components::tokio::TokioComponent)")
)]
pub struct TracingEndpoint {
    #[allow(dead_code)]
    addr: Option<SocketAddr>,
}

#[cfg(feature = "filter-reload")]
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
        if !cfg!(feature = "filter-reload") {
            warn!(addr = ?config.tracing.endpoint_addr,
                  "unable to activate configured tracing filter endpoint: \
                   enable the 'filter-reload' feature when compiling zebrad",
            );
        }

        Ok(Self {
            addr: config.tracing.endpoint_addr,
        })
    }

    #[cfg(feature = "filter-reload")]
    #[allow(clippy::unwrap_in_result)]
    pub fn init_tokio(&mut self, tokio_component: &TokioComponent) -> Result<(), FrameworkError> {
        use hyper::{
            service::{make_service_fn, service_fn},
            Server,
        };

        let addr = if let Some(addr) = self.addr {
            addr
        } else {
            return Ok(());
        };

        let service =
            make_service_fn(|_| async { Ok::<_, hyper::Error>(service_fn(request_handler)) });

        info!("Trying to open tracing endpoint at {}...", addr);
        tokio_component
            .rt
            .as_ref()
            .expect("runtime should not be taken")
            .spawn(async move {
                // try_bind uses the tokio runtime, so we
                // need to construct it inside the task.
                let server = match Server::try_bind(&addr) {
                    Ok(s) => s,
                    Err(e) => panic!(
                        "Opening tracing endpoint listener {:?} failed: {:?}. \
                         Hint: Check if another zebrad or zcashd process is running. \
                         Try changing the tracing endpoint_addr in the Zebra config.",
                        addr, e,
                    ),
                }
                .serve(service);

                info!("Opened tracing endpoint at {}", server.local_addr());

                if let Err(e) = server.await {
                    error!("Server error: {}", e);
                }
            });

        Ok(())
    }
}

#[cfg(feature = "filter-reload")]
#[instrument]
async fn request_handler(req: Request<Body>) -> Result<Response<Body>, hyper::Error> {
    use hyper::{Method, StatusCode};

    use super::Tracing;

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
