//! Tests for the HTTP request compatibility middleware.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use http_body::Frame;
use http_body_util::BodyExt;
use jsonrpsee::{
    core::BoxError,
    server::{stop_channel, HttpBody, HttpRequest, HttpResponse, ServerBuilder},
    RpcModule,
};
use jsonrpsee_types::{
    error::{
        OVERSIZED_RESPONSE_CODE, OVERSIZED_RESPONSE_MSG, TOO_BIG_BATCH_RESPONSE_CODE,
        TOO_BIG_BATCH_RESPONSE_MSG,
    },
    ErrorObject,
};
use serde_json::{json, value::RawValue, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};
use tower::Service;

use crate::server::http_request_compatibility::{
    HttpRequestMiddleware, HttpRequestMiddlewareLayer, JsonRpcRequest,
};

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

    let mut middleware = HttpRequestMiddleware::new(
        MockRpcService,
        None,
        2_097_152,
        usize::try_from(crate::config::rpc::Config::default().max_response_body_size).unwrap(),
    );
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

    let mut middleware = HttpRequestMiddleware::new(
        MockRpcService,
        None,
        limit,
        usize::try_from(crate::config::rpc::Config::default().max_response_body_size).unwrap(),
    );
    let result = middleware.call(request).await;

    assert!(result.is_err(), "oversized request body should be rejected");
}

/// The rewrite keeps an explicit `"id": null`, and still omits a missing `id` (a notification).
#[test]
fn rewrite_keeps_null_request_id() {
    let rewrite = |request: &str| {
        let request: JsonRpcRequest<'_> = serde_json::from_str(request).expect("valid request");
        serde_json::to_value(request.into_2()).expect("serializable")
    };

    let rewritten = rewrite(r#"{"jsonrpc":"2.0","method":"getinfo","params":[],"id":null}"#);
    assert_eq!(rewritten.get("id"), Some(&serde_json::Value::Null));

    let rewritten = rewrite(r#"{"jsonrpc":"2.0","method":"getinfo","params":[]}"#);
    assert_eq!(rewritten.get("id"), None);
}

/// Sends a request through the middleware and the real JSON-RPC dispatcher.
async fn rpc_response(body: &str) -> Value {
    let bytes = rpc_response_bytes(
        body,
        crate::config::rpc::Config::default().max_response_body_size,
    )
    .await
    .expect("request succeeds");
    serde_json::from_slice(&bytes).expect("the dispatcher returns JSON")
}

/// Collects the real dispatcher's response after compatibility conversion.
async fn rpc_response_bytes(body: &str, max_response_body_size: u32) -> Result<Bytes, BoxError> {
    let response = rpc_http_response(body, max_response_body_size).await?;
    Ok(response.into_body().collect().await?.to_bytes())
}

/// Keeps the real dispatcher's response limit independent from the request limit.
async fn rpc_http_response(
    body: &str,
    max_response_body_size: u32,
) -> Result<HttpResponse, BoxError> {
    let mut module = RpcModule::new(());
    module
        .register_method("echo", |params, _, _| params.one::<Value>())
        .expect("the method name is unique");
    module
        .register_method("echo_raw", |params, _, _| params.one::<Box<RawValue>>())
        .expect("the method name is unique");
    module
        .register_method("raw_error", |params, _, _| {
            let data = params.one::<Box<RawValue>>()?;
            Err::<(), _>(ErrorObject::owned(-32000, "raw error", Some(data)))
        })
        .expect("the method name is unique");
    let (stop_handle, _server_handle) = stop_channel();
    let service = ServerBuilder::default()
        .max_response_body_size(max_response_body_size)
        .to_service_builder()
        .build(module, stop_handle);
    let mut middleware = HttpRequestMiddleware::new(
        service,
        None,
        2_097_152,
        usize::try_from(max_response_body_size).expect("response limits fit in usize"),
    );
    let request = HttpRequest::builder()
        .method("POST")
        .body(HttpBody::from(body.to_owned()))
        .expect("the request has a valid method and body");
    middleware.call(request).await
}

#[tokio::test]
async fn versionless_batch_returns_legacy_results() {
    let response = rpc_response(
        r#"[{"method":"echo","params":["first"],"id":0},
            {"method":"echo","params":[null],"id":1},
            {"method":"echo","params":["legacy"],"id":null}]"#,
    )
    .await;
    assert_eq!(
        response,
        json!([
            {"result":"first","error":null,"id":0},
            {"result":null,"error":null,"id":1},
            {"result":"legacy","error":null,"id":null}
        ])
    );
}

/// Adding the version field must not let a legacy batch exceed the request size budget.
#[tokio::test]
async fn legacy_batch_rewrite_respects_request_body_limit() {
    let body = r#"[{"method":"echo","params":[],"id":0}]"#;
    let rewritten = r#"[{"jsonrpc":"2.0","method":"echo","params":[],"id":0}]"#;
    for limit in [body.len(), rewritten.len() - 1, rewritten.len()] {
        let request = HttpRequest::builder()
            .method("POST")
            .body(HttpBody::from(body.to_owned()))
            .expect("the request has a valid method and body");
        let mut middleware = HttpRequestMiddleware::new(
            MockRpcService,
            None,
            limit,
            usize::try_from(crate::config::rpc::Config::default().max_response_body_size).unwrap(),
        );
        assert_eq!(
            middleware.call(request).await.is_ok(),
            limit == rewritten.len(),
            "the normalized batch must fit within {limit} bytes"
        );
    }
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
    for body in [
        r#"{"jsonrpc":"2.0","method":"echo","params":["notification"]}"#,
        r#"[{"jsonrpc":"2.0","method":"echo","params":["notification"]}]"#,
    ] {
        assert!(rpc_response_bytes(
            body,
            crate::config::rpc::Config::default().max_response_body_size,
        )
        .await
        .expect("notifications succeed")
        .is_empty());
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

/// Legacy envelopes must fit the configured limit after conversion, not just before it.
#[tokio::test]
async fn legacy_response_rewrite_respects_response_body_limit() {
    for version in [None, Some("1.0")] {
        let mut requests = json!([
            {"method":"echo","params":["first"],"id":0},
            {"method":"echo","params":[null],"id":null},
            {"method":"missing","params":[],"id":2}
        ]);
        let mut expected = json!([
            {"result":"first","id":0,"error":null},
            {"result":null,"id":null,"error":null},
            {"result":null,"error":{"code":-32601,"message":"Method not found"},"id":2}
        ]);
        let original = json!([
            {"jsonrpc":"2.0","result":"first","id":0},
            {"jsonrpc":"2.0","result":null,"id":null},
            {"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":2}
        ]);
        for index in 0..3 {
            if let Some(version) = version {
                requests[index]["jsonrpc"] = json!(version);
                expected[index]["jsonrpc"] = json!(version);
            }
        }

        for index in [3, 0, 1, 2] {
            let (request, expected, original) = if index == 3 {
                (&requests, &expected, &original)
            } else {
                (&requests[index], &expected[index], &original[index])
            };
            let request = request.to_string();
            let original_len = original.to_string().len();
            let rewritten_len = expected.to_string().len();
            let limit = u32::try_from(original_len.max(rewritten_len))
                .expect("small response fixtures fit in u32");
            let response = rpc_response_bytes(&request, limit)
                .await
                .expect("both response envelopes fit the configured limit");
            assert_eq!(
                serde_json::from_slice::<Value>(&response).expect("response is JSON"),
                *expected,
                "version {version:?}, case {index}"
            );
            assert_eq!(response.len(), rewritten_len);
        }
    }
}

/// Conversion overflow must return a legacy RPC error over HTTP, not close the connection.
#[tokio::test]
async fn legacy_response_overflow_returns_http_error() {
    let requests = json!([
        {"jsonrpc":"2.0","method":"echo","params":["first"],"id":"pool\"\\id"},
        {"jsonrpc":"2.0","method":"echo","params":[null],"id":null},
        {"jsonrpc":"2.0","method":"missing","params":[],"id":2}
    ]);
    let original = json!([
        {"jsonrpc":"2.0","result":"first","id":"pool\"\\id"},
        {"jsonrpc":"2.0","result":null,"id":null},
        {"jsonrpc":"2.0","error":{"code":-32601,"message":"Method not found"},"id":2}
    ]);
    for index in [3, 0, 1, 2] {
        let (request, expected) = if index == 3 {
            (&requests, &original)
        } else {
            (&requests[index], &original[index])
        };
        let limit = expected.to_string().len();
        let mut legacy = request.clone();
        if let Some(batch) = legacy.as_array_mut() {
            for request in batch {
                request["jsonrpc"] = json!("1.0");
            }
        } else {
            legacy["jsonrpc"] = json!("1.0");
        }

        let mut module = RpcModule::new(());
        module
            .register_method("echo", |params, _, _| params.one::<Value>())
            .expect("the method name is unique");
        let middleware = tower::ServiceBuilder::new()
            .layer(HttpRequestMiddlewareLayer::new(None, 2_097_152, limit));
        let server = timeout(
            Duration::from_secs(5),
            ServerBuilder::default()
                .http_only()
                .set_http_middleware(middleware)
                .max_response_body_size(u32::try_from(limit).expect("small response fixture"))
                .build("127.0.0.1:0"),
        )
        .await
        .expect("binding the local HTTP server must finish")
        .expect("the local HTTP server binds");
        let address = server.local_addr().expect("the server has a bound address");
        let handle = server.start(module);
        let responses = timeout(Duration::from_secs(5), async {
            let mut responses = Vec::new();
            for request in [request.to_string(), legacy.to_string()] {
                let mut stream = TcpStream::connect(address).await?;
                let request = format!(
                    "POST / HTTP/1.1\r\nHost: {address}\r\nContent-Type: application/json\r\n\
                     Content-Length: {}\r\nConnection: close\r\n\r\n{request}",
                    request.len()
                );
                stream.write_all(request.as_bytes()).await?;
                let mut response = Vec::new();
                stream.read_to_end(&mut response).await?;
                responses.push(response);
            }
            Ok::<_, std::io::Error>(responses)
        })
        .await;
        handle.stop().expect("the server is still running");
        timeout(Duration::from_secs(5), handle.stopped())
            .await
            .expect("the local HTTP server must stop");

        let responses = responses
            .expect("HTTP requests must finish")
            .expect("HTTP connections can be read");
        let (code, message, id) = if index == 3 {
            (
                TOO_BIG_BATCH_RESPONSE_CODE,
                TOO_BIG_BATCH_RESPONSE_MSG,
                Value::Null,
            )
        } else {
            (
                OVERSIZED_RESPONSE_CODE,
                OVERSIZED_RESPONSE_MSG,
                expected["id"].clone(),
            )
        };
        let error = json!({
            "jsonrpc":"1.0",
            "id":id,
            "result":null,
            "error":{
                "code":code,
                "message":message,
                "data":format!("Exceeded max limit of {limit}")
            }
        });
        for (response, expected) in responses.iter().zip([expected, &error]) {
            let response = std::str::from_utf8(response).expect("HTTP response is UTF-8");
            let (headers, body) = response
                .split_once("\r\n\r\n")
                .expect("conversion overflow must return an HTTP response, not disconnect");
            assert!(headers.starts_with("HTTP/1.1 200 "), "{headers}");
            assert_eq!(
                serde_json::from_str::<Value>(body).expect("HTTP body is a JSON-RPC response"),
                *expected,
                "case {index}"
            );
        }
    }
}

/// jsonrpsee's oversized-response errors remain deliverable even above a small configured limit.
#[tokio::test]
async fn legacy_response_limits_preserve_oversized_error_envelopes() {
    for limit in [64, 140] {
        for version in [None, Some("1.0")] {
            let mut request = json!({"method":"echo","params":["x".repeat(limit)],"id":null});
            if let Some(version) = version {
                request["jsonrpc"] = json!(version);
            }
            // Two oversized method responses force the aggregate batch error at either limit.
            for (request, code) in [
                (request.to_string(), OVERSIZED_RESPONSE_CODE),
                (
                    json!([&request, &request]).to_string(),
                    TOO_BIG_BATCH_RESPONSE_CODE,
                ),
            ] {
                let response = rpc_http_response(&request, u32::try_from(limit).unwrap())
                    .await
                    .expect("the dispatcher's oversized-error envelope is still deliverable");
                assert_eq!(response.status(), hyper::StatusCode::OK);
                let bytes = response.into_body().collect().await.unwrap().to_bytes();
                let response: Value = serde_json::from_slice(&bytes).expect("response is JSON");
                assert_eq!(
                    response["error"]["code"], code,
                    "version {version:?}, limit {limit}"
                );
                assert_eq!(response.get("result"), Some(&Value::Null));
                assert_eq!(response.get("id"), Some(&Value::Null));
                assert_eq!(
                    response.get("jsonrpc"),
                    version.map(|version| json!(version)).as_ref()
                );
            }
        }
    }
}

/// Legacy normalization changes only envelopes, not raw parameters, results, or error data.
#[tokio::test]
async fn compatibility_preserves_raw_payloads() {
    let payload = r#"{ "same": 1e+02, "same": "\u0061", "nested": [ true, null ] }"#;
    let limit = crate::config::rpc::Config::default().max_response_body_size;
    for prefix in [
        "",
        r#""jsonrpc":"1.0","#,
        r#""jsonrpc":"\u0031.0","#,
        r#""jsonrpc":"2.0","#,
    ] {
        for method in ["echo_raw", "raw_error"] {
            let request =
                format!(r#"{{{prefix}"method":"{method}","params":[{payload}],"id":null}}"#);
            let batch_request = format!("[{request}]");
            for (request, batch) in [(&request, false), (&batch_request, true)] {
                let bytes = rpc_response_bytes(request, limit)
                    .await
                    .expect("request succeeds");
                let response = if batch {
                    serde_json::from_slice::<Vec<&RawValue>>(&bytes).expect("batch response")[0]
                } else {
                    serde_json::from_slice::<&RawValue>(&bytes).expect("single response")
                };
                let fields: std::collections::BTreeMap<&str, &RawValue> =
                    serde_json::from_str(response.get()).expect("response fields");
                let actual = if method == "echo_raw" {
                    fields["result"]
                } else {
                    let error: std::collections::BTreeMap<&str, &RawValue> =
                        serde_json::from_str(fields["error"].get()).expect("error fields");
                    error["data"]
                };
                assert_eq!(actual.get(), payload, "{request}");
            }
        }
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
        r#"[{"jsonrpc":"1.0","jsonrpc":"1.0","method":"echo","params":["called"],"id":1}]"#,
        r#"[{"method":"echo","params":[],"params":["called"],"id":1}]"#,
        r#"[{"method":"echo","params":["called"],"id":null,"id":1}]"#,
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
