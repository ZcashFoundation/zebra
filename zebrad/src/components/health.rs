//! A simple HTTP health endpoint for Zebra.

use std::net::SocketAddr;

use abscissa_core::{Component, FrameworkError};
use serde::{Deserialize, Serialize};
use tokio::spawn;
use hyper::{Request, Response, StatusCode};
use hyper::body::Incoming;
use hyper::service::service_fn;
use std::convert::Infallible;
use tracing::{info, error};
use crate::config::ZebradConfig;

/// Abscissa component which runs a health endpoint.
#[derive(Debug, Component)]
#[cfg(feature = "health-endpoint")]
pub struct HealthEndpoint {}

#[cfg(feature = "health-endpoint")]
impl HealthEndpoint {
    /// Create the component.
    pub fn new(config: &Config) -> Result<Self, FrameworkError> {
        if let Some(addr) = config.endpoint_addr {
            info!("Trying to open health endpoint at {}...", addr);

            // Start the health endpoint server in a separate thread to avoid Tokio runtime issues
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    if let Err(e) = Self::run_server(addr).await {
                        error!("Health endpoint server failed: {}", e);
                    }
                });
            });

            info!("Opened health endpoint at {}", addr);
        }

        Ok(Self {})
    }

    async fn run_server(addr: SocketAddr) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        
        loop {
            let (stream, _) = listener.accept().await?;
            let io = hyper_util::rt::TokioIo::new(stream);
            
            spawn(async move {
                if let Err(err) = hyper::server::conn::http1::Builder::new()
                    .serve_connection(io, service_fn(Self::handle_request))
                    .await
                {
                    error!("Health endpoint connection error: {}", err);
                }
            });
        }
    }

    async fn handle_request(req: Request<Incoming>) -> Result<Response<String>, Infallible> {
        // Check if the request is for the health endpoint
        if req.uri().path() != "/health" {
            return Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .header("Content-Type", "application/json")
                .body("{\"error\": \"Not Found\"}".to_string())
                .unwrap());
        }

        let health_info = HealthInfo {
            status: "healthy".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            git_tag: option_env!("GIT_TAG").unwrap_or("unknown").to_string(),
            git_commit: option_env!("GIT_COMMIT_FULL").unwrap_or("unknown").to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        };

        let response_body = serde_json::to_string_pretty(&health_info)
            .unwrap_or_else(|_| "{\"error\": \"Failed to serialize health info\"}".to_string());

        Ok(Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(response_body)
            .unwrap())
    }
}



/// Health information response.
#[derive(Debug, Serialize)]
struct HealthInfo {
    status: String,
    version: String,
    git_tag: String,
    git_commit: String,
    timestamp: String,
}

/// Health endpoint configuration section.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields, default)]
#[cfg(feature = "health-endpoint")]
pub struct Config {
    /// The address used for the health endpoint.
    ///
    /// The endpoint is disabled if this is set to `None`.
    pub endpoint_addr: Option<SocketAddr>,
}

#[cfg(feature = "health-endpoint")]
impl Default for Config {
    fn default() -> Self {
        Self {
            endpoint_addr: Some("127.0.0.1:8080".parse().unwrap()),
        }
    }
}
