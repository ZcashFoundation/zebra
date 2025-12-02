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

use std::time::Instant;

use jsonrpsee::{
    server::middleware::rpc::{layer::ResponseFuture, RpcServiceT},
    MethodResponse,
};

/// Middleware that collects Prometheus metrics for each RPC request.
///
/// This middleware records:
/// - `rpc.requests.total{method, status}` - Counter of requests by method and status
/// - `rpc.request.duration_seconds{method}` - Histogram of request durations
/// - `rpc.active_requests` - Gauge of currently active requests
/// - `rpc.errors.total{method, error_code}` - Counter of errors by method and code
#[derive(Clone)]
pub struct RpcMetricsMiddleware<S> {
    service: S,
}

impl<S> RpcMetricsMiddleware<S> {
    /// Create a new `RpcMetricsMiddleware` with the given `service`.
    pub fn new(service: S) -> Self {
        Self { service }
    }
}

impl<'a, S> RpcServiceT<'a> for RpcMetricsMiddleware<S>
where
    S: RpcServiceT<'a> + Send + Sync + Clone + 'static,
{
    type Future = ResponseFuture<futures::future::BoxFuture<'a, MethodResponse>>;

    fn call(&self, request: jsonrpsee::types::Request<'a>) -> Self::Future {
        let service = self.service.clone();
        let method = request.method_name().to_owned();
        let start = Instant::now();

        // Increment active requests gauge
        metrics::gauge!("rpc.active_requests").increment(1.0);

        ResponseFuture::future(Box::pin(async move {
            let response = service.call(request).await;
            let duration = start.elapsed().as_secs_f64();

            // Determine status and record metrics
            let status = if response.is_error() { "error" } else { "success" };

            // Record request count
            metrics::counter!("rpc.requests.total", "method" => method.clone(), "status" => status)
                .increment(1);

            // Record request duration
            metrics::histogram!("rpc.request.duration_seconds", "method" => method.clone())
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
