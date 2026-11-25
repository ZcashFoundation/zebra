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
    ErrorObject, Id, Response, ResponsePayload,
};
use proptest::prelude::*;
use serde_json::{json, value::RawValue, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
};
use tower::{service_fn, Service, ServiceExt};

use crate::server::http_request_compatibility::{
    is_canonical_json_rpc_2_request, is_canonical_submitblock_request, reencode_response,
    response_to_version, splice_response, HttpRequestMiddleware, HttpRequestMiddlewareLayer,
    JsonRpcRequest, JsonRpcVersion,
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

/// Sends `request` through the middleware to a service that replies with `response`, and
/// returns the request body the service received and the response body the client got.
async fn round_trip(request: &str, response: &'static str) -> (String, String) {
    let (sender, mut received) = tokio::sync::mpsc::unbounded_channel();
    let inner = service_fn(move |request: HttpRequest| {
        let sender = sender.clone();
        async move {
            let body = request.into_body().collect().await?.to_bytes();
            sender.send(body).expect("receiver is alive");
            Ok::<_, BoxError>(HttpResponse::new(HttpBody::from(response)))
        }
    });
    let mut middleware = HttpRequestMiddleware::new(
        inner,
        None,
        2_097_152,
        usize::try_from(crate::config::rpc::Config::default().max_response_body_size).unwrap(),
    );

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

    let received = received.try_recv().expect("called");
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

/// Common JSON-RPC 2.0 client layouts go straight to the dispatcher without re-encoding.
#[tokio::test]
async fn submitblock_fast_path_accepts_common_layouts() {
    let response = r#"{"jsonrpc":"2.0","result":null,"id":1}"#;
    for request in [
        r#"{"jsonrpc":"2.0","method":"submitblock","params":["aB"],"id":1}"#,
        // Python json.dumps with its default separators.
        r#"{"jsonrpc": "2.0", "id": 1, "method": "submitblock", "params": ["aB"]}"#,
        // Node JSON.stringify with the version key in the last position.
        r#"{"method":"submitblock","params":["aB"],"id":1,"jsonrpc":"2.0"}"#,
        r#"{"id":1,"params":["aB"],"method":"submitblock","jsonrpc":"2.0"}"#,
        "{\r\n\"id\"\t:\t\"pool\",\n\"params\": [ \"aB\" ], \"jsonrpc\":\"2.0\", \"method\":\"submitblock\" }",
        r#"{"params":[""],"method":"submitblock","id":null,"jsonrpc":"2.0"}"#,
        r#"{"params":["f"],"method":"submitblock","id":18446744073709551615,"jsonrpc":"2.0"}"#,
    ] {
        assert!(is_canonical_submitblock_request(request.as_bytes()), "{request}");
        assert_eq!(
            round_trip(request, response).await,
            (request.to_string(), response.to_string()),
            "{request}"
        );
    }

    let large = "aB".repeat(1_900_000);
    let body = format!(r#"{{"id":1,"params":["{large}"],"method":"submitblock","jsonrpc":"2.0"}}"#);
    assert!(is_canonical_submitblock_request(body.as_bytes()));
}

/// Inputs outside the exact shortcut still use the existing JSON-RPC compatibility path.
#[tokio::test]
async fn submitblock_fast_path_rejects_noncanonical_layouts() {
    for body in [
        r#"{"id":1,"method":"submitblock","params":["00"]}"#,
        // Versionless Node JSON.stringify requests retain bitcoind compatibility.
        r#"{"method":"submitblock","params":["00"],"id":1}"#,
        r#"{"jsonrpc":"1.0","method":"submitblock","params":["00"],"id":1}"#,
        r#"{"jsonrpc":"2.0","jsonrpc":"2.0","method":"submitblock","params":["00"],"id":1}"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","method":"submitblock","params":["00"],"id":1}"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","params":["00"],"id":1,"x":0}"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","params":["00\u0030"],"id":1}"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","params":["00","11"],"id":1}"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","params":[0],"id":1}"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","params":["00",],"id":1}"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","params":["00"],"id":1,}"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","params":["00"],"id":1}garbage"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","params":["00"],"id":"a\n"}"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","params":["00"],"id":[1]}"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","params":["00"],"id":01}"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","params":["00"],"id":18446744073709551616}"#,
        r#"{"jsonrpc":"2.0","method":"submitblock","params":["gg"],"id":1}"#,
    ] {
        assert!(!is_canonical_submitblock_request(body.as_bytes()), "{body}");
    }
    let response = r#"{"jsonrpc":"2.0","result":null,"id":1}"#;
    let legacy = r#"{"id":1,"method":"submitblock","params":["00"]}"#;
    let (forwarded, reply) = round_trip(legacy, response).await;
    assert_eq!(
        json(&forwarded),
        json(r#"{"jsonrpc":"2.0","id":1,"method":"submitblock","params":["00"]}"#)
    );
    assert_eq!(json(&reply), json(r#"{"result":null,"error":null,"id":1}"#));
}

/// Parses what the submitblock handler reads, including the second optional argument.
fn submitblock_handler_reads(body: &[u8]) -> (Id<'static>, String, Option<String>) {
    let request: jsonrpsee_types::Request<'_> =
        serde_json::from_slice(body).expect("valid JSON-RPC request");
    assert_eq!(request.method, "submitblock");
    let params = request.params.as_ref().expect("submitblock has params");
    let params = jsonrpsee_types::Params::new(Some(params.get()));
    let mut sequence = params.sequence();
    let hex = sequence
        .next::<String>()
        .expect("first parameter is a string");
    let second = sequence
        .optional_next::<String>()
        .expect("there is no second parameter");
    (request.id.into_owned(), hex, second)
}

/// Every accepted wire shape has the same meaning when the generic path re-encodes it.
#[tokio::test]
async fn submitblock_fast_path_matches_generic_forwarding() {
    let response = r#"{"jsonrpc":"2.0","result":null,"id":1}"#;
    for request in [
        r#"{"jsonrpc":"2.0","id":1,"method":"submitblock","params":["aB"]}"#,
        r#"{"params": [ "f" ], "id": "miner", "method": "submitblock", "jsonrpc": "2.0"}"#,
        r#"{"id":null,"jsonrpc":"2.0","params":[""],"method":"submitblock"}"#,
        r#"{"params":[ "aB" ],"method":"submitblock","id":null,"jsonrpc":"2.0"}"#,
        r#"{"id":"é","method":"submitblock","params":["00"],"jsonrpc":"2.0"}"#,
        r#"{"params":["00"],"jsonrpc":"2.0","method":"submitblock","id":18446744073709551615}"#,
    ] {
        assert!(
            is_canonical_submitblock_request(request.as_bytes()),
            "{request}"
        );
        let (forwarded, _) = round_trip(request, response).await;
        let decoded: Value = serde_json::from_str(request).expect("valid request");
        let generic = serde_json::to_vec(&decoded).expect("generic path decodes params");
        assert_eq!(
            submitblock_handler_reads(forwarded.as_bytes()),
            submitblock_handler_reads(&generic),
            "{request}"
        );
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn submitblock_random_layouts_read_like_generic(
        hex in "[0-9a-fA-F]{0,48}",
        id in prop_oneof![
            Just("null".to_string()),
            any::<u64>().prop_map(|id| id.to_string()),
            "[a-zA-Z0-9]{0,16}".prop_map(|id| format!("\"{id}\"")),
        ],
        order in any::<u8>(),
        between in prop::sample::select(vec!["", " ", "\t", "\r\n"]),
        inside in prop::sample::select(vec!["", " ", "\t", "\n"]),
    ) {
        let mut fields = [
            format!(r#""jsonrpc"{between}:"2.0""#),
            format!(r#""method"{between}:"submitblock""#),
            format!(r#""params"{between}:[{inside}"{hex}"{inside}]"#),
            format!(r#""id"{between}:{id}"#),
        ];
        fields.rotate_left(usize::from(order % 4));
        if order & 4 != 0 {
            fields.swap(0, 2);
        }
        if order & 8 != 0 {
            fields.swap(1, 3);
        }
        let body = format!("{{{}}}", fields.join(&format!("{between},{between}")));
        prop_assert!(is_canonical_submitblock_request(body.as_bytes()), "{body}");

        let decoded: Value = serde_json::from_str(&body).expect("generated JSON is valid");
        let generic = serde_json::to_vec(&decoded).expect("generic path decodes params");
        prop_assert_eq!(
            submitblock_handler_reads(body.as_bytes()),
            submitblock_handler_reads(&generic)
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

/// The 1.0 versions whose responses are converted.
const CONVERTED: [JsonRpcVersion; 2] = [JsonRpcVersion::Lightwalletd, JsonRpcVersion::Bitcoind];

/// Serializes a response the way the `jsonrpsee` server does.
fn jsonrpsee_response(id: Id<'_>, payload: ResponsePayload<'_, Value>) -> Bytes {
    serde_json::to_vec(&Response::new(payload, id))
        .expect("serializable")
        .into()
}

/// Asserts that the splice converts `body` to the same bytes as parsing and re-encoding it.
fn assert_splice_matches_reencode(body: &Bytes) {
    for version in CONVERTED {
        let spliced = splice_response(body, version);
        assert_eq!(
            spliced.as_deref().map(String::from_utf8_lossy),
            Some(String::from_utf8_lossy(
                &reencode_response(body.clone(), version, usize::MAX)
                    .expect("unlimited conversion succeeds")
            )),
            "{version:?} {}",
            String::from_utf8_lossy(&body[..body.len().min(200)]),
        );
    }
}

fn ids() -> Vec<Id<'static>> {
    vec![
        Id::Null,
        Id::Number(0),
        Id::Number(u64::MAX),
        Id::Str("lwd".into()),
        Id::Str("".into()),
        Id::Str("q\"b\\s\n\u{1}\u{7f}é/,}".into()),
        Id::Str("\"jsonrpc\":\"2.0\",\"error\":null".into()),
        Id::Str("a".repeat(300).into()),
    ]
}

/// Every `jsonrpsee` response layout takes the splice, with the same output as the parsing path.
#[test]
fn spliced_responses_match_reencoded_responses() {
    let results = [
        json!(null),
        json!(true),
        json!(0),
        json!(-1),
        json!(1.5),
        json!(1e300),
        json!(u64::MAX),
        json!(""),
        json!("null"),
        json!("}"),
        json!("\"jsonrpc\":\"2.0\",\"id\":1,\"error\":null}"),
        json!("ab".repeat(1 << 20)),
        json!([]),
        json!({}),
        json!([null, "a\\", {"error": {"code": 1}}]),
        json!({"jsonrpc": "2.0", "id": 1, "error": null, "result": {"tx": ["00", 1, -2.5]}}),
        json!({"hash": "00", "nested": [[[{"é": "\u{0}"}]]]}),
    ];
    let errors = [
        ErrorObject::owned(-8, "bad", None::<()>),
        ErrorObject::owned(-32700, "Parse error", None::<()>),
        ErrorObject::owned(
            0,
            "\"error\":\"result\"\n",
            Some(json!({"result": null, "z": 1, "a": [1.0]})),
        ),
        ErrorObject::owned(i32::MIN, "", Some("x".repeat(1 << 16))),
    ];

    for id in ids() {
        for result in &results {
            let body = jsonrpsee_response(id.clone(), ResponsePayload::success(result.clone()));
            assert_splice_matches_reencode(&body);
        }
        for error in &errors {
            let body = jsonrpsee_response(id.clone(), ResponsePayload::error(error.clone()));
            assert_splice_matches_reencode(&body);
        }
    }
}

/// Any other layout falls back to parsing, so the output is still the parsing path's.
#[test]
fn unexpected_layouts_fall_back_to_reencoding() {
    for body in [
        r#"{"jsonrpc":"2.0","result":1,"id":1}"#,
        r#"{"id":1,"jsonrpc":"2.0","result":1}"#,
        r#"{"jsonrpc": "2.0","id":1,"result":1}"#,
        r#"{"jsonrpc":"2.0","id":1,"result": 1}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":1 }"#,
        r#"{"jsonrpc":"2.0","id":1,"result":{"a":1} }"#,
        r#"{"jsonrpc":"2.0","id":1,"result":"a" }"#,
        r#"{"jsonrpc":"2.0","id":1,"result":{"a":1}} "#,
        r#"{"jsonrpc":"2.0","id":1,"result":1,"error":null}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":"#,
        r#"{"jsonrpc":"2.0","id":1,"result":}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":"}"#,
        r#"{"jsonrpc":"2.0","id":1,"result":{"a":1}"#,
        r#"{"jsonrpc":"2.0","id":1,"other":1}"#,
        r#"{"jsonrpc":"2.0","id":1}"#,
        r#"{"jsonrpc":"2.0","id":01,"result":1}"#,
        r#"{"jsonrpc":"2.0","id":"a,"result":1}"#,
        r#"{"jsonrpc":"2.0","id":[1,2],"result":1}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":1},"result":1}"#,
        r#"{"jsonrpc":"2.0","id":1,"error":{"code":1}"#,
        r#"{"jsonrpc":"1.0","id":1,"result":1}"#,
        r#"[{"jsonrpc":"2.0","id":1,"result":1}]"#,
        r#"not json {"#,
        "",
    ] {
        let body = Bytes::from(body.to_string());
        for version in CONVERTED {
            assert_eq!(splice_response(&body, version), None, "{body:?}");
            assert_eq!(
                response_to_version(body.clone(), version, usize::MAX)
                    .expect("unlimited conversion succeeds"),
                reencode_response(body.clone(), version, usize::MAX)
                    .expect("unlimited conversion succeeds")
            );
        }
    }
}

/// A JSON value of bounded depth with arbitrary scalars, strings and keys.
fn any_json() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>().prop_map(Value::from),
        any::<String>().prop_map(Value::from),
    ];
    leaf.prop_recursive(4, 32, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::from),
            prop::collection::btree_map(any::<String>(), inner, 0..6)
                .prop_map(|map| Value::Object(map.into_iter().collect())),
        ]
    })
}

fn any_id() -> impl Strategy<Value = Id<'static>> {
    prop_oneof![
        Just(Id::Null),
        any::<u64>().prop_map(Id::Number),
        ".{0,40}".prop_map(|id: String| Id::Str(id.into())),
    ]
}

proptest! {
    #[test]
    fn spliced_success_matches_reencoded(id in any_id(), result in any_json()) {
        assert_splice_matches_reencode(&jsonrpsee_response(id, ResponsePayload::success(result)));
    }

    #[test]
    fn spliced_error_matches_reencoded(
        id in any_id(),
        code in any::<i32>(),
        message in any::<String>(),
        data in prop::option::of(any_json()),
    ) {
        let error = ErrorObject::owned(code, message, data);
        assert_splice_matches_reencode(&jsonrpsee_response(id, ResponsePayload::error(error)));
    }
}

/// The middleware converts a 1.0 caller's large response by splicing, byte for byte.
#[tokio::test]
async fn large_json_rpc_1_response_is_spliced() {
    let result = "ab".repeat(1 << 20);
    let response: &'static str =
        Box::leak(format!(r#"{{"jsonrpc":"2.0","id":7,"result":"{result}"}}"#).into_boxed_str());
    let (_, lwd) = round_trip(
        r#"{"jsonrpc":"1.0","method":"getblock","params":["1",0],"id":7}"#,
        response,
    )
    .await;
    assert_eq!(
        lwd,
        format!(r#"{{"jsonrpc":"1.0","id":7,"result":"{result}","error":null}}"#)
    );

    let (_, cli) = round_trip(r#"{"method":"getblock","params":["1",0],"id":7}"#, response).await;
    assert_eq!(
        cli,
        format!(r#"{{"id":7,"result":"{result}","error":null}}"#)
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
            // Decode and re-encode, as the slow path does: re-encoding `JsonRpcRequest` would copy
            // its raw params and id byte for byte, so the comparison could never fail.
            let decoded: Value =
                serde_json::from_slice(&body).expect("canonical requests are valid JSON");
            let re_encoded = serde_json::to_vec(&decoded).expect("serializable");
            prop_assert_eq!(jsonrpsee_reads(&body), jsonrpsee_reads(&re_encoded));
            prop_assert!(jsonrpsee_reads(&body).is_some(), "jsonrpsee reads every canonical call");
        }
    }
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
                // Single 2.0 calls that aren't canonical are re-encoded, see
                // `non_canonical_json_rpc_2_is_re_encoded`. 2.0 batches still pass through.
                if prefix.contains("2.0") && !batch {
                    continue;
                }
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
