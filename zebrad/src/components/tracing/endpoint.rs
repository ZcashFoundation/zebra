//! An HTTP endpoint for dynamically setting tracing filters.

#![allow(non_local_definitions)]

use std::net::SocketAddr;

use abscissa_core::{Component, FrameworkError};

use crate::config::ZebradConfig;

#[cfg(feature = "filter-reload")]
use hyper::{
    body::{Body, Incoming},
    Method, Request, Response, StatusCode,
};
#[cfg(feature = "filter-reload")]
use hyper_util::{
    rt::{TokioExecutor, TokioIo},
    server::conn::auto::Builder,
};

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
async fn read_filter(req: Request<impl Body>) -> Result<String, String> {
    use http_body_util::BodyExt;

    std::str::from_utf8(
        req.into_body()
            .collect()
            .await
            .map_err(|_| "Error reading body".to_owned())?
            .to_bytes()
            .as_ref(),
    )
    .map(|s| s.to_owned())
    .map_err(|_| "Filter must be UTF-8".to_owned())
}

impl TracingEndpoint {
    /// Create the component.
    pub fn new(config: &ZebradConfig) -> Result<Self, FrameworkError> {
        if config.tracing.endpoint_addr.is_some() && !cfg!(feature = "filter-reload") {
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
        let addr = if let Some(addr) = self.addr {
            addr
        } else {
            return Ok(());
        };

        info!("Trying to open tracing endpoint at {}...", addr);

        let svc = hyper::service::service_fn(|req: Request<Incoming>| async move {
            request_handler(req).await
        });

        tokio_component
            .rt
            .as_ref()
            .expect("runtime should not be taken")
            .spawn(async move {
                let listener = match tokio::net::TcpListener::bind(addr).await {
                    Ok(listener) => listener,
                    Err(err) => {
                        panic!(
                            "Opening tracing endpoint listener {addr:?} failed: {err:?}. \
                            Hint: Check if another zebrad or zcashd process is running. \
                            Try changing the tracing endpoint_addr in the Zebra config.",
                        );
                    }
                };
                info!(
                    "Opened tracing endpoint at {}",
                    listener
                        .local_addr()
                        .expect("Local address must be available as the bind was successful")
                );

                while let Ok((stream, _)) = listener.accept().await {
                    let io = TokioIo::new(stream);
                    tokio::spawn(async move {
                        if let Err(err) = Builder::new(TokioExecutor::new())
                            .serve_connection(io, svc)
                            .await
                        {
                            error!(
                                "Serve connection in {addr:?} failed: {err:?}.",
                                addr = addr,
                                err = err
                            );
                        }
                    });
                }
            });

        Ok(())
    }
}

#[cfg(feature = "filter-reload")]
#[instrument]
async fn request_handler(req: Request<Incoming>) -> Result<Response<String>, hyper::Error> {
    use super::Tracing;

    let rsp = match (req.method(), req.uri().path()) {
        (&Method::GET, "/") => Response::new(
            r#"
This HTTP endpoint allows dynamic control of the filter applied to
tracing events.

To get the current filter, GET /filter:

    curl -X GET localhost:3000/filter

To set the filter, POST the new filter string to /filter:

    curl -X POST localhost:3000/filter -d "zebrad=trace"
"#
            .to_string(),
        ),
        (&Method::GET, "/filter") => Response::builder()
            .status(StatusCode::OK)
            .body(
                APPLICATION
                    .state()
                    .components()
                    .get_downcast_ref::<Tracing>()
                    .expect("Tracing component should be available")
                    .filter(),
            )
            .expect("response with known status code cannot fail"),
        (&Method::POST, "/filter") => match read_filter(req).await {
            Ok(filter) => {
                APPLICATION
                    .state()
                    .components()
                    .get_downcast_ref::<Tracing>()
                    .expect("Tracing component should be available")
                    .reload_filter(filter);

                Response::new("".to_string())
            }
            Err(e) => Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(e)
                .expect("response with known status code cannot fail"),
        },
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body("".to_string())
            .expect("response with known status cannot fail"),
    };
    Ok(rsp)
}
