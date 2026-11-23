//! RPC metrics middleware for Prometheus metrics collection.
//!
//! This middleware collects metrics for JSON-RPC requests, including:
//! - Request count by method and status
//! - Request duration by method
//! - Active request count
//! - Error count by method and error code
//!
//! These metrics complement the OpenTelemetry tracing in `rpc_tracing.rs`,
//! providing aggregated data suitable for dashboards and alerting.

use std::{collections::HashSet, sync::Arc, time::Instant};

use jsonrpsee::{
    server::middleware::rpc::{layer::ResponseFuture, RpcServiceT},
    MethodResponse,
};

#[cfg(test)]
mod tests;

/// Middleware that collects Prometheus metrics for each RPC request.
///
/// This middleware records:
/// - `rpc.requests.total{method, status}` - Counter of requests by method and status
/// - `rpc.request.duration_seconds{method}` - Histogram of request durations
/// - `rpc.active_requests` - Gauge of currently active requests
/// - `rpc.errors.total{method, error_code}` - Counter of errors by method and code
///
/// Method labels use registered RPC method names, or `unknown` for unregistered names.
#[derive(Clone)]
pub struct RpcMetricsMiddleware<S> {
    service: S,
    registered_methods: Arc<HashSet<&'static str>>,
}

impl<S> RpcMetricsMiddleware<S> {
    /// Create a new `RpcMetricsMiddleware` with the given `service` and registered method names.
    ///
    /// Collect `registered_methods` from the server's `RpcModule::method_names()` iterator.
    /// Requests for names outside this set are labeled `unknown`.
    pub fn new(service: S, registered_methods: Arc<HashSet<&'static str>>) -> Self {
        Self {
            service,
            registered_methods,
        }
    }

    fn method_label(&self, method: &str) -> &'static str {
        method_label(&self.registered_methods, method)
    }
}

/// The registered name of `method`, or `unknown` for names the server does not register, so
/// labels stay bounded whatever clients send.
pub(crate) fn method_label(
    registered_methods: &HashSet<&'static str>,
    method: &str,
) -> &'static str {
    registered_methods.get(method).copied().unwrap_or("unknown")
}

impl<'a, S> RpcServiceT<'a> for RpcMetricsMiddleware<S>
where
    S: RpcServiceT<'a> + Send + Sync + Clone + 'static,
{
    type Future = ResponseFuture<futures::future::BoxFuture<'a, MethodResponse>>;

    fn call(&self, request: jsonrpsee::types::Request<'a>) -> Self::Future {
        let service = self.service.clone();
        let method = self.method_label(request.method_name());
        let start = Instant::now();

        // Increment active requests gauge
        metrics::gauge!("rpc.active_requests").increment(1.0);

        ResponseFuture::future(Box::pin(async move {
            let response = service.call(request).await;
            let duration = start.elapsed().as_secs_f64();

            // Determine status and record metrics
            let status = if response.is_error() {
                "error"
            } else {
                "success"
            };

            // Record request count
            metrics::counter!("rpc.requests.total", "method" => method, "status" => status)
                .increment(1);

            // Record request duration
            metrics::histogram!("rpc.request.duration_seconds", "method" => method)
                .record(duration);

            // Record errors with error code
            if response.is_error() {
                let error_code = response
                    .as_error_code()
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                metrics::counter!(
                    "rpc.errors.total",
                    "method" => method,
                    "error_code" => error_code
                )
                .increment(1);
            }

            // Decrement active requests gauge
            metrics::gauge!("rpc.active_requests").decrement(1.0);

            response
        }))
    }
}
