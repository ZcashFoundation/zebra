//! Tests for the HTTP request compatibility middleware.

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};

use bytes::Bytes;
use http_body::Frame;
use http_body_util::BodyExt;
use jsonrpsee::{
    core::BoxError,
    server::{HttpBody, HttpRequest, HttpResponse},
};
use jsonrpsee_types::Id;
use proptest::prelude::*;
use serde_json::Value;
use tower::{service_fn, Service, ServiceExt};

use crate::server::http_request_compatibility::{
    is_canonical_json_rpc_2_request, HttpRequestMiddleware, JsonRpcRequest,
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

/// The rewrite keeps an explicit `"id": null`, and still omits a missing `id` (a notification).
#[test]
fn rewrite_keeps_null_request_id() {
    let rewrite = |request: &str| {
        let request: JsonRpcRequest = serde_json::from_str(request).expect("valid request");
        serde_json::to_value(request.into_2()).expect("serializable")
    };

    let rewritten = rewrite(r#"{"jsonrpc":"2.0","method":"getinfo","params":[],"id":null}"#);
    assert_eq!(rewritten.get("id"), Some(&serde_json::Value::Null));

    let rewritten = rewrite(r#"{"jsonrpc":"2.0","method":"getinfo","params":[]}"#);
    assert_eq!(rewritten.get("id"), None);
}

/// Sends `request` through the middleware to a service that replies with `response`, and
/// returns the request body the service received and the response body the client got.
async fn round_trip(request: &str, response: &'static str) -> (String, String) {
    let received = Arc::new(Mutex::new(None));
    let inner = {
        let received = received.clone();
        service_fn(move |request: HttpRequest| {
            let received = received.clone();
            async move {
                let body = request.into_body().collect().await?.to_bytes();
                *received.lock().expect("unpoisoned") = Some(body);
                Ok::<_, BoxError>(HttpResponse::new(HttpBody::from(response)))
            }
        })
    };
    let mut middleware = HttpRequestMiddleware::new(inner, None, 2_097_152);

    let response = middleware
        .ready()
        .await
        .expect("ready")
        .call(HttpRequest::new(HttpBody::from(request.to_string())))
        .await
        .expect("response");
    let response = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();

    let received = received.lock().expect("unpoisoned").take().expect("called");
    (
        String::from_utf8(received.to_vec()).expect("utf8"),
        String::from_utf8(response.to_vec()).expect("utf8"),
    )
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).expect("valid json")
}

/// A canonical 2.0 request and its response pass through byte for byte, even with unusual key order.
#[tokio::test]
async fn canonical_json_rpc_2_passes_through_unchanged() {
    let response = r#"{"jsonrpc":"2.0","result":null,"id":"a"}"#;
    for request in [
        r#"{"id":"a","params":["00ab",2,-1,true,false,null],"method":"getblock","jsonrpc":"2.0"}"#,
        r#"{"jsonrpc":"2.0","method":"getinfo","params":[],"id":18446744073709551615}"#,
        r#"{"jsonrpc":"2.0","method":"getinfo"}"#,
    ] {
        assert_eq!(
            round_trip(request, response).await,
            (request.to_string(), response.to_string())
        );
    }
}

/// 2.0 requests that re-encoding would change are still re-encoded, and their response passes through.
#[tokio::test]
async fn non_canonical_json_rpc_2_is_re_encoded() {
    let response = r#"{"jsonrpc":"2.0","result":1,"id":1}"#;
    for (request, expected) in [
        // `jsonrpsee` rejects `[ ]` as params, but accepts `[]`.
        (
            r#"{"jsonrpc":"2.0","method":"getinfo","params":[ ],"id":1}"#,
            r#"{"jsonrpc":"2.0","method":"getinfo","params":[],"id":1}"#,
        ),
        (
            r#" {"jsonrpc": "2.0", "method": "getblock", "params": ["1", 2], "id": 1}"#,
            r#"{"jsonrpc":"2.0","method":"getblock","params":["1",2],"id":1}"#,
        ),
        (
            r#"{"jsonrpc":"2.0","method":"getinfo","params":["A",1.50],"id":"1"}"#,
            r#"{"jsonrpc":"2.0","method":"getinfo","params":["A",1.5],"id":"1"}"#,
        ),
        (
            r#"{"jsonrpc":"2.0","method":"getaddressbalance","params":[{"a": 2}],"id":1,"x":0}"#,
            r#"{"jsonrpc":"2.0","method":"getaddressbalance","params":[{"a":2}],"id":1}"#,
        ),
    ] {
        assert_eq!(
            round_trip(request, response).await,
            (expected.to_string(), response.to_string())
        );
    }
}

/// `jsonrpsee` reads positional params `[ ]` as an error but `[]` as empty, so spacing matters.
#[test]
fn jsonrpsee_params_depend_on_spacing() {
    let next = |params| {
        jsonrpsee_types::Params::new(Some(params))
            .sequence()
            .optional_next::<u32>()
    };
    assert_eq!(next("[]").expect("empty params"), None);
    assert!(next("[ ]").is_err());
}

/// lightwalletd and zcash-cli requests are converted to 2.0, and responses back to their version.
#[tokio::test]
async fn json_rpc_1_is_converted_both_ways() {
    let (request, response) = round_trip(
        r#"{"jsonrpc":"1.0","method":"getblock","params":["1",2],"id":"lwd"}"#,
        r#"{"jsonrpc":"2.0","result":{"hash":"00"},"id":"lwd"}"#,
    )
    .await;
    assert_eq!(
        request,
        r#"{"jsonrpc":"2.0","method":"getblock","params":["1",2],"id":"lwd"}"#
    );
    assert_eq!(
        json(&response),
        json(r#"{"jsonrpc":"1.0","result":{"hash":"00"},"error":null,"id":"lwd"}"#)
    );

    let (request, response) = round_trip(
        r#"{"method":"getinfo","params":[],"id":1}"#,
        r#"{"jsonrpc":"2.0","error":{"code":-8,"message":"bad"},"id":1}"#,
    )
    .await;
    assert_eq!(
        request,
        r#"{"jsonrpc":"2.0","method":"getinfo","params":[],"id":1}"#
    );
    assert_eq!(
        json(&response),
        json(r#"{"result":null,"error":{"code":-8,"message":"bad"},"id":1}"#)
    );
}

/// An explicit `"id": null` is kept on both the pass-through and the conversion paths.
#[tokio::test]
async fn null_request_id_is_kept() {
    let request = r#"{"jsonrpc":"2.0","method":"getinfo","params":[],"id":null}"#;
    let response = r#"{"jsonrpc":"2.0","result":1,"id":null}"#;
    assert_eq!(
        round_trip(request, response).await,
        (request.to_string(), response.to_string())
    );

    let (request, response) = round_trip(
        r#"{"jsonrpc":"1.0","method":"getinfo","params":[],"id":null}"#,
        response,
    )
    .await;
    assert_eq!(
        request,
        r#"{"jsonrpc":"2.0","method":"getinfo","params":[],"id":null}"#
    );
    assert_eq!(
        json(&response),
        json(r#"{"jsonrpc":"1.0","result":1,"error":null,"id":null}"#)
    );
}

/// Batches and non-JSON bodies reach the server as sent, and their responses come back as sent.
#[tokio::test]
async fn batch_and_non_json_pass_through_unchanged() {
    let batch = r#"[{"jsonrpc":"1.0","method":"getinfo","params":[],"id":1}, {"jsonrpc":"2.0","method":"getinfo","id":2}]"#;
    let batch_response =
        r#"[{"jsonrpc":"2.0","result":1,"id":1},{"jsonrpc":"2.0","result":2,"id":2}]"#;
    assert_eq!(
        round_trip(batch, batch_response).await,
        (batch.to_string(), batch_response.to_string())
    );

    let garbage = "not json {";
    let error = r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"Parse error"},"id":null}"#;
    assert_eq!(
        round_trip(garbage, error).await,
        (garbage.to_string(), error.to_string())
    );
}

/// JSON text for one request field value, including the spellings re-encoding would change.
fn any_value_text() -> impl Strategy<Value = String> {
    let scalar = prop_oneof![
        Just("true".to_string()),
        Just("false".to_string()),
        Just("null".to_string()),
        Just("-0".to_string()),
        Just("1.0".to_string()),
        Just("1e2".to_string()),
        Just("18446744073709551616".to_string()),
        any::<u64>().prop_map(|n| n.to_string()),
        any::<i64>().prop_map(|n| n.to_string()),
        any::<f64>().prop_map(|n| Value::from(n).to_string()),
        any::<String>().prop_map(|s| Value::from(s).to_string()),
        "[a-z0-9 ,\\]\\[]{0,8}".prop_map(|s| Value::from(s).to_string()),
    ];
    scalar.prop_recursive(2, 16, 4, |inner| {
        (prop::collection::vec(inner.clone(), 0..4), any_separator())
            .prop_map(|(items, sep)| format!("[{}]", items.join(sep)))
    })
}

fn any_separator() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just(","), Just(","), Just(", "), Just(" ,")]
}

/// A request body: the canonical shape most of the time, with each part sometimes made unusual.
fn any_request_body() -> impl Strategy<Value = Vec<u8>> {
    let jsonrpc = prop_oneof![
        Just(r#""2.0""#.to_string()),
        Just(r#""2.0""#.to_string()),
        Just(r#""1.0""#.to_string()),
        Just(r#""2.00""#.to_string()),
        Just("2.0".to_string()),
    ];
    let method = prop_oneof![
        "[a-z_]{1,20}".prop_map(|m| Value::from(m).to_string()),
        any::<String>().prop_map(|m| Value::from(m).to_string()),
    ];
    let params = prop_oneof![
        (
            prop::collection::vec(any_value_text(), 0..6),
            any_separator()
        )
            .prop_map(|(items, sep)| Some(format!("[{}]", items.join(sep)))),
        Just(Some("[ ]".to_string())),
        Just(Some("null".to_string())),
        Just(Some(r#"{"a":1}"#.to_string())),
        Just(None),
    ];
    let id = prop::option::of(any_value_text());
    let extra = prop::option::of(Just(r#""x":0"#.to_string()));
    let fields = (
        jsonrpc,
        method,
        params,
        id,
        extra,
        any::<bool>(),
        any::<u64>(),
    )
        .prop_map(|(jsonrpc, method, params, id, extra, space, order)| {
            let mut fields = vec![
                format!(r#""jsonrpc":{jsonrpc}"#),
                format!(r#""method":{method}"#),
            ];
            fields.extend(params.map(|p| format!(r#""params":{p}"#)));
            fields.extend(id.map(|i| format!(r#""id":{i}"#)));
            fields.extend(extra);
            let n = fields.len();
            fields.rotate_left((order % n as u64) as usize);
            let colon = if space { ", " } else { "," };
            format!("{{{}}}", fields.join(colon)).into_bytes()
        })
        .boxed();
    prop_oneof![
        8 => fields.clone(),
        1 => fields.prop_map(|body| [b" ".as_slice(), &body].concat()),
        1 => prop::collection::vec(any::<u8>(), 0..64),
    ]
}

/// What `jsonrpsee` reads from a request or notification body: method, id and raw params text.
fn jsonrpsee_reads(body: &[u8]) -> Option<(String, Option<Id<'static>>, Option<String>)> {
    if let Ok(request) = serde_json::from_slice::<jsonrpsee_types::Request<'_>>(body) {
        return Some((
            request.method.to_string(),
            Some(request.id.into_owned()),
            request.params.map(|params| params.get().to_string()),
        ));
    }
    let notification: jsonrpsee_types::Notification<'_, Option<&serde_json::value::RawValue>> =
        serde_json::from_slice(body).ok()?;
    Some((
        notification.method.to_string(),
        None,
        notification.params.map(|params| params.get().to_string()),
    ))
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(16_384))]

    /// A request forwarded unchanged reads exactly as the re-encoded request the slow path sends.
    #[test]
    fn pass_through_requests_read_like_re_encoded_ones(body in any_request_body()) {
        if is_canonical_json_rpc_2_request(&body) {
            let request: JsonRpcRequest =
                serde_json::from_slice(&body).expect("canonical requests are valid requests");
            let re_encoded = serde_json::to_vec(&request.into_2()).expect("serializable");
            prop_assert_eq!(jsonrpsee_reads(&body), jsonrpsee_reads(&re_encoded));
            prop_assert!(jsonrpsee_reads(&body).is_some(), "jsonrpsee reads every canonical call");
        }
    }
}
