//! RPC tracing middleware for OpenTelemetry SERVER spans.
//!
//! This middleware enables Jaeger Service Performance Monitoring (SPM) by marking
//! JSON-RPC endpoints with `SPAN_KIND_SERVER`. SPM displays RED metrics (Rate, Errors,
//! Duration) for server-side handlers.
//!
//! # Background
//!
//! Jaeger SPM filters for `SPAN_KIND_SERVER` spans by default. Without this middleware,
//! all Zebra spans are `SPAN_KIND_INTERNAL`, making them invisible in SPM's Monitor tab.
//!
//! This follows OpenTelemetry best practices used by other blockchain clients:
//! - Lighthouse (Ethereum consensus client)
//! - Hyperledger Besu (Ethereum execution client)
//! - Hyperledger Fabric

use jsonrpsee::{
    server::middleware::rpc::{layer::ResponseFuture, RpcServiceT},
    MethodResponse,
};
use tracing::{info_span, Instrument};

/// Middleware that creates SERVER spans for each RPC request.
///
/// This enables Jaeger SPM by marking RPC endpoints as server-side handlers.
/// Also captures error codes and messages for debugging.
///
/// # OpenTelemetry Attributes
///
/// Each span includes:
/// - `otel.kind = "server"` - Marks this as a server span for SPM
/// - `rpc.method` - The JSON-RPC method name (e.g., "getinfo", "getblock")
/// - `rpc.system = "jsonrpc"` - The RPC protocol
/// - `otel.status_code` - "ERROR" on failure (empty on success)
/// - `rpc.error_code` - JSON-RPC error code on failure
#[derive(Clone)]
pub struct RpcTracingMiddleware<S> {
    service: S,
}

impl<S> RpcTracingMiddleware<S> {
    /// Create a new `RpcTracingMiddleware` with the given `service`.
    pub fn new(service: S) -> Self {
        Self { service }
    }
}

impl<'a, S> RpcServiceT<'a> for RpcTracingMiddleware<S>
where
    S: RpcServiceT<'a> + Send + Sync + Clone + 'static,
{
    type Future = ResponseFuture<futures::future::BoxFuture<'a, MethodResponse>>;

    fn call(&self, request: jsonrpsee::types::Request<'a>) -> Self::Future {
        let service = self.service.clone();
        let method = request.method_name().to_owned();

        // Create span OUTSIDE the async block so it's properly registered
        // with the tracing subscriber before the future starts.
        let span = info_span!(
            "rpc_request",
            otel.kind = "server",
            rpc.method = %method,
            rpc.system = "jsonrpc",
            otel.status_code = tracing::field::Empty,
            rpc.error_code = tracing::field::Empty,
        );

        // Clone span for recording after response
        let span_for_record = span.clone();

        // Instrument the ENTIRE future with the span
        ResponseFuture::future(Box::pin(
            async move {
                let response = service.call(request).await;

                // Record error details if the response is an error
                if response.is_error() {
                    span_for_record.record("otel.status_code", "ERROR");
                    if let Some(error_code) = response.as_error_code() {
                        span_for_record.record("rpc.error_code", error_code);
                    }
                }

                response
            }
            .instrument(span),
        ))
    }
}
