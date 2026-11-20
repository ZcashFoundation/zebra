//! Tests for the HTTP request compatibility middleware.

use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use bytes::Bytes;
use http_body::Frame;
use jsonrpsee::{
    core::BoxError,
    server::{HttpBody, HttpRequest, HttpResponse},
};
use tower::Service;

use base64::{engine::general_purpose::STANDARD, Engine as _};

use crate::server::{
    cookie::Cookie,
    http_request_compatibility::{HttpRequestMiddleware, JsonRpcRequest, RpcAuth},
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

/// Builds a middleware that requires the given credentials.
fn auth_middleware(auth: Option<RpcAuth>) -> HttpRequestMiddleware<MockRpcService> {
    HttpRequestMiddleware::new(MockRpcService, auth, 2_097_152)
}

/// Builds the header map of a request sent with `basic_auth(user, password)`.
fn basic_auth_headers(user: &str, password: &str) -> hyper::HeaderMap {
    let encoded = STANDARD.encode(format!("{user}:{password}"));
    let mut headers = hyper::HeaderMap::new();
    headers.insert(
        hyper::header::AUTHORIZATION,
        format!("Basic {encoded}").parse().unwrap(),
    );
    headers
}

fn static_auth() -> RpcAuth {
    RpcAuth::Static {
        user: "user".to_string(),
        password: Cookie::from_secret("s3cret:with:colons".to_string()),
    }
}

#[test]
fn no_auth_accepts_everything() {
    let middleware = auth_middleware(None);
    assert!(middleware.check_credentials(&hyper::HeaderMap::new()));
    assert!(middleware.check_credentials(&basic_auth_headers("anyone", "anything")));
}

#[test]
fn static_auth_accepts_matching_user_and_password() {
    let middleware = auth_middleware(Some(static_auth()));
    assert!(middleware.check_credentials(&basic_auth_headers("user", "s3cret:with:colons")));
}

#[test]
fn static_auth_rejects_wrong_or_missing_credentials() {
    let middleware = auth_middleware(Some(static_auth()));
    assert!(!middleware.check_credentials(&hyper::HeaderMap::new()));
    assert!(!middleware.check_credentials(&basic_auth_headers("user", "wrong")));
    assert!(!middleware.check_credentials(&basic_auth_headers("other", "s3cret:with:colons")));
    assert!(!middleware.check_credentials(&basic_auth_headers("user", "s3cret:with")));
    assert!(!middleware.check_credentials(&basic_auth_headers("", "")));

    let mut garbage = hyper::HeaderMap::new();
    garbage.insert(
        hyper::header::AUTHORIZATION,
        "Basic not-base64!".parse().unwrap(),
    );
    assert!(!middleware.check_credentials(&garbage));
}

/// The cookie only checks the password half, for compatibility with `zcash-cli`.
#[test]
fn cookie_auth_ignores_username() {
    let cookie = Cookie::from_secret("cookie-secret".to_string());
    let middleware = auth_middleware(Some(RpcAuth::Cookie(cookie)));
    assert!(middleware.check_credentials(&basic_auth_headers("__cookie__", "cookie-secret")));
    assert!(middleware.check_credentials(&basic_auth_headers("whatever", "cookie-secret")));
    assert!(!middleware.check_credentials(&basic_auth_headers("__cookie__", "wrong")));
}
