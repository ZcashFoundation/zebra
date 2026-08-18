//! Request tracing for the lightwalletd gRPC server.
//!
//! The generated `CompactTxStreamer` service has no logging of its own, so a node
//! serving light clients gives no sign of what it was asked for, even at `trace`. This
//! layer logs every request and its outcome, in the same shape as the JSON-RPC
//! server's [`RpcTracingMiddleware`](crate::server::rpc_tracing::RpcTracingMiddleware):
//! a `SERVER` span per request carrying the method name, so traces from both servers
//! group the same way in Jaeger.

use std::{
    task::{Context, Poll},
    time::Instant,
};

use futures::future::BoxFuture;
use tonic::codegen::http;
use tower::{Layer, Service};
use tracing::{debug, info_span, Instrument};

/// Adds a [`GrpcTracing`] to a gRPC service.
#[derive(Clone, Copy, Debug, Default)]
pub struct GrpcTracingLayer;

impl<S> Layer<S> for GrpcTracingLayer {
    type Service = GrpcTracing<S>;

    fn layer(&self, service: S) -> Self::Service {
        GrpcTracing { service }
    }
}

/// Logs each gRPC request and its outcome, inside a span naming the method.
#[derive(Clone, Debug)]
pub struct GrpcTracing<S> {
    service: S,
}

impl<S, RequestBody, ResponseBody> Service<http::Request<RequestBody>> for GrpcTracing<S>
where
    S: Service<http::Request<RequestBody>, Response = http::Response<ResponseBody>>
        + Clone
        + Send
        + 'static,
    S::Future: Send + 'static,
    RequestBody: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx)
    }

    fn call(&mut self, request: http::Request<RequestBody>) -> Self::Future {
        let method = grpc_method(request.uri().path()).to_string();

        // Build the span outside the async block, so it is registered with the
        // subscriber before the future is polled.
        let span = info_span!(
            "lightwalletd_grpc_request",
            otel.kind = "server",
            rpc.method = %method,
            rpc.system = "grpc",
            rpc.grpc.status_code = tracing::field::Empty,
        );
        let span_for_record = span.clone();

        // `poll_ready` was called on `self.service`, so the readiness belongs to it and
        // not to a clone. Swap the ready service out and leave the clone behind.
        let clone = self.service.clone();
        let mut service = std::mem::replace(&mut self.service, clone);

        Box::pin(
            async move {
                debug!("handling lightwalletd gRPC request");

                let start = Instant::now();
                let response = service.call(request).await;
                let elapsed_ms = start.elapsed().as_millis();

                match &response {
                    Ok(response) => {
                        // tonic reports the final status in the trailers, for streaming and
                        // unary responses alike, and those are not visible here. The header
                        // is only set when the request failed before the handler ran, so a
                        // status is logged when there is one, and left out otherwise.
                        let status = response
                            .headers()
                            .get("grpc-status")
                            .and_then(|status| status.to_str().ok());

                        match status {
                            Some(status) => {
                                span_for_record.record("rpc.grpc.status_code", status);
                                debug!(
                                    grpc_status = status,
                                    elapsed_ms, "rejected lightwalletd gRPC request",
                                );
                            }
                            // A streaming method responds as soon as its headers are sent,
                            // so this is the time to the first response, not to the last
                            // message.
                            None => debug!(elapsed_ms, "responded to lightwalletd gRPC request"),
                        }
                    }
                    Err(_) => debug!(
                        elapsed_ms,
                        "lightwalletd gRPC request failed before a response was produced",
                    ),
                }

                response
            }
            .instrument(span),
        )
    }
}

/// Returns the method name from a gRPC request path.
///
/// tonic routes on `/<package>.<service>/<Method>`, so the method is the last segment.
fn grpc_method(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grpc_method_is_the_last_path_segment() {
        assert_eq!(
            grpc_method("/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetBlock"),
            "GetBlock",
        );
        // Reflection and any other service the server hosts are named the same way.
        assert_eq!(
            grpc_method("/grpc.reflection.v1.ServerReflection/ServerReflectionInfo"),
            "ServerReflectionInfo",
        );
        // A path that isn't a gRPC route must not panic or return an empty name.
        assert_eq!(grpc_method("nonsense"), "nonsense");
    }

    /// Logging a request must not change it, or consume the service's readiness.
    ///
    /// `call` hands the polled-ready service to the response future and leaves a clone
    /// behind, so a second request has to be served as well as the first.
    #[tokio::test]
    async fn requests_pass_through_unchanged() {
        use tower::ServiceExt;

        let inner = tower::service_fn(|_request: http::Request<()>| async {
            Ok::<_, std::convert::Infallible>(
                http::Response::builder()
                    .status(200)
                    .body(())
                    .expect("a 200 response is valid"),
            )
        });

        let mut service = GrpcTracingLayer.layer(inner);

        for _ in 0..2 {
            let request = http::Request::builder()
                .uri("/cash.z.wallet.sdk.rpc.CompactTxStreamer/GetBlock")
                .body(())
                .expect("the request is valid");

            let response = service
                .ready()
                .await
                .expect("the service is always ready")
                .call(request)
                .await
                .expect("the inner service always succeeds");

            assert_eq!(response.status(), 200);
        }
    }
}
