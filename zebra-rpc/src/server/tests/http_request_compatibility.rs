//! Tests for the HTTP request compatibility middleware.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use http_body::Frame;
use http_body_util::BodyExt;
use jsonrpsee::{
    core::BoxError,
    server::{stop_channel, HttpBody, HttpRequest, HttpResponse, ServerBuilder},
    RpcModule,
};
use serde_json::{json, Value};
use tower::Service;

use crate::server::http_request_compatibility::HttpRequestMiddleware;

/// A body that always returns an error, simulating a TCP RST during body collection.
struct ErrorBody;

impl http_body::Body for ErrorBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Poll::Ready(Some(Err("connection reset".into())))
    }
}

/// A mock inner service that returns a minimal JSON-RPC 2.0 response.
#[derive(Clone)]
struct MockRpcService;

impl Service<HttpRequest> for MockRpcService {
    type Response = HttpResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, BoxError>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: HttpRequest) -> Self::Future {
        let body = r#"{"jsonrpc":"2.0","id":1,"result":null}"#;
        let response = HttpResponse::new(HttpBody::from(body.to_string()));
        Box::pin(async { Ok(response) })
    }
}

/// Verifies that body collection errors return `Err` instead of panicking.
///
/// Previously, the middleware called `.expect()` on `body.collect().await`,
/// so a TCP RST during body reading would panic the process.
#[tokio::test]
async fn request_body_error_returns_err_instead_of_panic() {
    let error_body = HttpBody::new(ErrorBody);
    let request = HttpRequest::builder()
        .method("POST")
        .header("content-type", "appliion/json")
        .body(error_body)
        .expect("valid request");

    let mut middleware = HttpRequestMiddleware::new(MockRpcService, None, 2_097_152);
    let result = middleware.call(request).await;

    assert!(
        result.is_err(),
        "body collection error should return Err, not panic"
    );
}

/// Verifies that a request body exceeding `max_request_body_size` is rejected.
#[tokio::test]
async fn oversized_request_body_is_rejected() {
    let limit = 64;
    let oversized = vec![b'x'; limit + 1];
    let body = HttpBody::from(oversized);
    let request = HttpRequest::builder()
        .method("POST")
        .header("content-type", "application/json")
        .body(body)
        .expect("valid request");

    let mut middleware = HttpRequestMiddleware::new(MockRpcService, None, limit);
    let result = middleware.call(request).await;

    assert!(result.is_err(), "oversized request body should be rejected");
}

/// Sends a request through the middleware and the real JSON-RPC dispatcher.
async fn rpc_response(body: &str) -> Value {
    let mut module = RpcModule::new(());
    module
        .register_method("echo", |params, _, _| params.one::<Value>())
        .expect("the method name is unique");
    let (stop_handle, _server_handle) = stop_channel();
    let service = ServerBuilder::default()
        .to_service_builder()
        .build(module, stop_handle);
    let mut middleware = HttpRequestMiddleware::new(service, None, 2_097_152);
    let request = HttpRequest::builder()
        .method("POST")
        .body(HttpBody::from(body.to_owned()))
        .expect("the request has a valid method and body");
    let response = middleware.call(request).await.expect("request succeeds");
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body can be collected")
        .to_bytes();
    serde_json::from_slice(&bytes).expect("the dispatcher returns JSON")
}

#[tokio::test]
async fn versionless_batch_returns_legacy_results() {
    let response = rpc_response(
        r#"[{"method":"echo","params":["first"],"id":0},
            {"method":"echo","params":[null],"id":1}]"#,
    )
    .await;
    assert_eq!(
        response,
        json!([
            {"result":"first","error":null,"id":0},
            {"result":null,"error":null,"id":1}
        ])
    );
}

#[tokio::test]
async fn two_point_zero_batches_preserve_null_ids() {
    for body in [
        r#"[{"jsonrpc":"2.0","method":"echo","params":["called"],"id":null}]"#,
        r#"[{"jsonrpc":"2.0","method":"echo","params":["called"],"id":null},
            {"jsonrpc":"2.0","method":"echo","params":["notification"]}]"#,
    ] {
        assert_eq!(
            rpc_response(body).await,
            json!([{"jsonrpc":"2.0","result":"called","id":null}])
        );
    }
}

#[tokio::test]
async fn single_and_batch_responses_preserve_the_request_version() {
    for version in [None, Some("1.0"), Some("2.0")] {
        let mut requests = json!([
            {"method":"echo","params":["first"],"id":0},
            {"method":"echo","params":[null],"id":1},
            {"method":"missing","params":[],"id":2}
        ]);
        let mut expected = json!([
            {"result":"first","id":0},
            {"result":null,"id":1},
            {"error":{"code":-32601,"message":"Method not found"},"id":2}
        ]);
        for index in 0..3 {
            if let Some(version) = version {
                requests[index]["jsonrpc"] = json!(version);
                expected[index]["jsonrpc"] = json!(version);
            }
            if version != Some("2.0") {
                expected[index][if index == 2 { "result" } else { "error" }] = Value::Null;
            }
            assert_eq!(
                rpc_response(&requests[index].to_string()).await,
                expected[index]
            );
        }
        assert_eq!(rpc_response(&requests.to_string()).await, expected);
    }
}

#[tokio::test]
async fn unsupported_batches_keep_normal_server_errors() {
    for body in [
        "[]",
        "[7]",
        r#"[{"method":7}]"#,
        r#"[{"jsonrpc":"3.0","method":"echo","params":[],"id":0}]"#,
        r#"[[null,"echo",[],1]]"#,
        r#"[{"method":"missing","method":"echo","params":["called"],"id":1}]"#,
        r#"[{"method":"echo","params":["legacy"],"id":null}]"#,
        r#"[{"method":"echo","params":["first"],"id":0},
            {"method":"echo","params":["legacy"],"id":null}]"#,
    ] {
        let response = rpc_response(body).await;
        let response = response.as_array().map_or(&response, |batch| &batch[0]);
        assert_eq!(response["error"]["code"], -32600, "{body}");
    }
    for body in ["not JSON", "[{"] {
        assert_eq!(rpc_response(body).await["error"]["code"], -32700, "{body}");
    }

    let response = rpc_response(
        r#"[{"method":"echo","params":["legacy"],"id":0},
            {"jsonrpc":"2.0","method":"echo","params":[null],"id":1}]"#,
    )
    .await;
    assert_eq!(response[0]["error"]["code"], -32600);
    assert_eq!(response[1], json!({"jsonrpc":"2.0","result":null,"id":1}));
}
