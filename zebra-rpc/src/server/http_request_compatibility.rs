//! Compatibility fixes for JSON-RPC HTTP requests.
//!
//! These fixes are applied at the HTTP level, before the RPC request is parsed.

use std::{fmt, future::Future, pin::Pin};

use futures::{future, FutureExt};
use http_body_util::{BodyExt, Limited};
use hyper::header;
use jsonrpsee::{
    core::BoxError,
    server::{HttpBody, HttpRequest, HttpResponse},
};
use jsonrpsee_types::ErrorObject;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tower::Service;

use super::cookie::Cookie;

use base64::{engine::general_purpose::STANDARD, Engine as _};

/// HTTP [`HttpRequestMiddleware`] with compatibility workarounds.
///
/// This middleware makes the following changes to HTTP requests:
///
/// ### Remove `jsonrpc` field in JSON RPC 1.0
///
/// Removes "jsonrpc: 1.0" fields from requests,
/// because the "jsonrpc" field was only added in JSON-RPC 2.0.
///
/// <http://www.simple-is-better.org/rpc/#differences-between-1-0-and-2-0>
///
/// ### Add missing `content-type` HTTP header
///
/// Some RPC clients don't include a `content-type` HTTP header.
/// But unlike web browsers, [`jsonrpsee`] does not do content sniffing.
///
/// If there is no `content-type` header, we assume the content is JSON,
/// and let the parser error if we are incorrect.
///
/// ### Authenticate incoming requests
///
/// If RPC authentication is enabled, check that the incoming request carries the
/// configured credentials: either the per-start authentication cookie, or the
/// static user and password from the config. Requests that fail the check get an
/// HTTP 401 response.
///
/// Cookie authentication enables compatibility with `zcash-cli`.
///
/// ## Security
///
/// Any user-specified data in RPC requests is hex or base58check encoded.
/// We assume lightwalletd validates data encodings before sending it on to Zebra.
/// So any fixes Zebra performs won't change user-specified data.
#[derive(Clone, Debug)]
pub struct HttpRequestMiddleware<S> {
    service: S,
    auth: Option<RpcAuth>,
    max_request_body_size: usize,
}

#[derive(Clone)]
pub(super) enum RpcAuth {
    Cookie(Cookie),
    Static([u8; 32]),
}

impl fmt::Debug for RpcAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cookie(_) => "Cookie(..)",
            Self::Static(_) => "Static(..)",
        })
    }
}

impl RpcAuth {
    pub(super) fn static_credentials(user: &str, password: &str) -> Self {
        // Compare fixed-size digests so even a length mismatch checks every byte.
        Self::Static(
            Sha256::new()
                .chain_update(user)
                .chain_update(b":")
                .chain_update(password)
                .finalize()
                .into(),
        )
    }

    fn authenticate(&self, authorization: &str) -> bool {
        let encoded = match self {
            // Preserve the header formats accepted by existing cookie clients.
            Self::Cookie(_) => authorization.split_whitespace().nth(1),
            Self::Static(_) => authorization
                .split_once(' ')
                .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Basic"))
                .map(|(_, value)| value.trim_start_matches(' ')),
        };
        let Some(credentials) = encoded.and_then(|value| STANDARD.decode(value).ok()) else {
            return false;
        };
        match self {
            Self::Cookie(cookie) => std::str::from_utf8(&credentials)
                .ok()
                .and_then(|pair| pair.split_once(':'))
                .is_some_and(|(_, password)| cookie.authenticate(password.to_owned())),
            Self::Static(expected) => expected.ct_eq(&Sha256::digest(credentials)).into(),
        }
    }
}

impl<S> HttpRequestMiddleware<S> {
    /// Create a new `HttpRequestMiddleware` with the given service, cookie, and request body size limit.
    pub fn new(service: S, cookie: Option<Cookie>, max_request_body_size: usize) -> Self {
        Self::with_auth(service, cookie.map(RpcAuth::Cookie), max_request_body_size)
    }

    /// Create a new `HttpRequestMiddleware` that requires the given credentials, if any.
    fn with_auth(service: S, auth: Option<RpcAuth>, max_request_body_size: usize) -> Self {
        Self {
            service,
            auth,
            max_request_body_size,
        }
    }

    /// Check if the request is authenticated.
    pub fn check_credentials(&self, headers: &header::HeaderMap) -> bool {
        self.auth.as_ref().is_none_or(|auth| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|auth_header| auth_header.to_str().ok())
                .is_some_and(|authorization| auth.authenticate(authorization))
        })
    }

    /// Returns the HTTP 401 response sent when authentication fails.
    ///
    /// The body is a JSON-RPC error object, so clients see why the request was refused
    /// instead of an empty reply.
    fn unauthenticated_response() -> HttpResponse<HttpBody> {
        let error = ErrorObject::borrowed(401, "unauthenticated method", None);
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "error": error,
            "id": null,
        });

        let mut response = HttpResponse::new(HttpBody::from(body.to_string()));
        *response.status_mut() = hyper::StatusCode::UNAUTHORIZED;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            header::HeaderValue::from_static("Basic realm=\"jsonrpc\""),
        );
        response
    }

    /// Insert or replace client supplied `content-type` HTTP header to `application/json` in the following cases:
    ///
    /// - no `content-type` supplied.
    /// - supplied `content-type` start with `text/plain`, for example:
    ///   - `text/plain`
    ///   - `text/plain;`
    ///   - `text/plain; charset=utf-8`
    ///
    /// `application/json` is the only `content-type` accepted by the Zebra rpc endpoint:
    ///
    /// <https://github.com/paritytech/jsonrpc/blob/38af3c9439aa75481805edf6c05c6622a5ab1e70/http/src/handler.rs#L582-L584>
    ///
    /// # Security
    ///
    /// - `content-type` headers exist so that applications know they are speaking the correct protocol with the correct format.
    ///   We can be a bit flexible, but there are some types (such as binary) we shouldn't allow.
    ///   In particular, the "application/x-www-form-urlencoded" header should be rejected, so browser forms can't be used to attack
    ///   a local RPC port. See "The Role of Routers in the CSRF Attack" in
    ///   <https://www.invicti.com/blog/web-security/importance-content-type-header-http-requests/>
    /// - Checking all the headers is secure, but only because hyper has custom code that just reads the first content-type header.
    ///   <https://github.com/hyperium/headers/blob/f01cc90cf8d601a716856bc9d29f47df92b779e4/src/common/content_type.rs#L102-L108>
    pub fn insert_or_replace_content_type_header(headers: &mut header::HeaderMap) {
        if !headers.contains_key(header::CONTENT_TYPE)
            || headers
                .get(header::CONTENT_TYPE)
                .filter(|value| {
                    value
                        .to_str()
                        .ok()
                        .unwrap_or_default()
                        .starts_with("text/plain")
                })
                .is_some()
        {
            headers.insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );
        }
    }

    /// Maps whatever JSON-RPC version the client is using to JSON-RPC 2.0.
    async fn request_to_json_rpc_2(
        request: HttpRequest<HttpBody>,
        max_request_body_size: usize,
    ) -> Result<(JsonRpcVersion, HttpRequest<HttpBody>), BoxError> {
        let (parts, body) = request.into_parts();
        let bytes = Limited::new(body, max_request_body_size)
            .collect()
            .await?
            .to_bytes();
        let (version, bytes) =
            if let Ok(request) = serde_json::from_slice::<'_, JsonRpcRequest>(bytes.as_ref()) {
                let version = request.version();
                if matches!(version, JsonRpcVersion::Unknown) {
                    (version, bytes)
                } else {
                    (
                        version,
                        serde_json::to_vec(&request.into_2()).expect("valid").into(),
                    )
                }
            } else {
                (JsonRpcVersion::Unknown, bytes)
            };
        Ok((
            version,
            // `Vec::from` reuses the buffer without copying when it is uniquely owned.
            HttpRequest::from_parts(parts, HttpBody::from(Vec::from(bytes))),
        ))
    }
    /// Maps JSON-2.0 to whatever JSON-RPC version the client is using.
    async fn response_from_json_rpc_2(
        version: JsonRpcVersion,
        response: HttpResponse<HttpBody>,
    ) -> Result<HttpResponse<HttpBody>, BoxError> {
        let (parts, body) = response.into_parts();
        let bytes = body.collect().await?.to_bytes();
        let bytes =
            if let Ok(response) = serde_json::from_slice::<'_, JsonRpcResponse>(bytes.as_ref()) {
                serde_json::to_vec(&response.into_version(version))
                    .expect("valid")
                    .into()
            } else {
                bytes
            };
        Ok(HttpResponse::from_parts(
            parts,
            HttpBody::from(bytes.as_ref().to_vec()),
        ))
    }
}

/// Implement the Layer for HttpRequestMiddleware to allow injecting the credentials
#[derive(Clone)]
pub struct HttpRequestMiddlewareLayer {
    auth: Option<RpcAuth>,
    max_request_body_size: usize,
}

impl HttpRequestMiddlewareLayer {
    /// Create a new `HttpRequestMiddlewareLayer` with the given cookie and request body size limit.
    pub fn new(cookie: Option<Cookie>, max_request_body_size: usize) -> Self {
        Self::with_auth(cookie.map(RpcAuth::Cookie), max_request_body_size)
    }

    /// Create a new `HttpRequestMiddlewareLayer` that requires the given credentials, if any.
    pub(super) fn with_auth(auth: Option<RpcAuth>, max_request_body_size: usize) -> Self {
        Self {
            auth,
            max_request_body_size,
        }
    }
}

impl<S> tower::Layer<S> for HttpRequestMiddlewareLayer {
    type Service = HttpRequestMiddleware<S>;

    fn layer(&self, service: S) -> Self::Service {
        HttpRequestMiddleware::with_auth(service, self.auth.clone(), self.max_request_body_size)
    }
}

impl<S> Service<HttpRequest<HttpBody>> for HttpRequestMiddleware<S>
where
    S: Service<HttpRequest, Response = HttpResponse> + std::clone::Clone + Send + 'static,
    S::Error: Into<BoxError> + 'static,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = BoxError;
    type Future =
        Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send + 'static>>;

    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.service.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, mut request: HttpRequest<HttpBody>) -> Self::Future {
        // Check if the request is authenticated
        if !self.check_credentials(request.headers_mut()) {
            return future::ok(Self::unauthenticated_response()).boxed();
        }

        // Fix the request headers.
        Self::insert_or_replace_content_type_header(request.headers_mut());

        let mut service = self.service.clone();
        let max_request_body_size = self.max_request_body_size;

        async move {
            let (version, request) =
                Self::request_to_json_rpc_2(request, max_request_body_size).await?;
            let response = service.call(request).await.map_err(Into::into)?;
            Self::response_from_json_rpc_2(version, response).await
        }
        .boxed()
    }
}

#[derive(Clone, Copy, Debug)]
enum JsonRpcVersion {
    /// bitcoind used a mishmash of 1.0, 1.1, and 2.0 for its JSON-RPC.
    Bitcoind,
    /// lightwalletd uses the above mishmash, but also breaks spec to include a
    /// `"jsonrpc": "1.0"` key.
    Lightwalletd,
    /// The client is indicating strict 2.0 handling.
    TwoPointZero,
    /// On parse errors we don't modify anything, and let the `jsonrpsee` crate handle it.
    Unknown,
}

/// A version-agnostic JSON-RPC request.
#[derive(Debug, Deserialize, Serialize)]
pub(super) struct JsonRpcRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    jsonrpc: Option<String>,
    method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<serde_json::Value>,
    #[serde(
        default,
        deserialize_with = "deserialize_present_id",
        skip_serializing_if = "Option::is_none"
    )]
    id: Option<serde_json::Value>,
}

fn deserialize_present_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<serde_json::Value>, D::Error> {
    serde_json::Value::deserialize(deserializer).map(Some)
}

impl JsonRpcRequest {
    fn version(&self) -> JsonRpcVersion {
        match (self.jsonrpc.as_deref(), &self.params, &self.id) {
            (
                Some("2.0"),
                _,
                None
                | Some(
                    serde_json::Value::Null
                    | serde_json::Value::String(_)
                    | serde_json::Value::Number(_),
                ),
            ) => JsonRpcVersion::TwoPointZero,
            (Some("1.0"), Some(_), Some(_)) => JsonRpcVersion::Lightwalletd,
            (None, Some(_), Some(_)) => JsonRpcVersion::Bitcoind,
            _ => JsonRpcVersion::Unknown,
        }
    }

    pub(super) fn into_2(mut self) -> Self {
        self.jsonrpc = Some("2.0".into());
        self
    }
}
/// A version-agnostic JSON-RPC response.
#[derive(Debug, Deserialize, Serialize)]
struct JsonRpcResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    jsonrpc: Option<String>,
    id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Box<serde_json::value::RawValue>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<serde_json::Value>,
}

impl JsonRpcResponse {
    fn into_version(mut self, version: JsonRpcVersion) -> Self {
        match version {
            JsonRpcVersion::Bitcoind => {
                self.jsonrpc = None;
                self.result = self
                    .result
                    .or_else(|| serde_json::value::to_raw_value(&()).ok());
                self.error = self.error.or(Some(serde_json::Value::Null));
            }
            JsonRpcVersion::Lightwalletd => {
                self.jsonrpc = Some("1.0".into());
                self.result = self
                    .result
                    .or_else(|| serde_json::value::to_raw_value(&()).ok());
                self.error = self.error.or(Some(serde_json::Value::Null));
            }
            JsonRpcVersion::TwoPointZero => {
                // `jsonrpsee` should be returning valid JSON-RPC 2.0 responses. However,
                // a valid result of `null` can be parsed into `None` by this parser, so
                // we map the result explicitly to `Null` when there is no error.
                assert_eq!(self.jsonrpc.as_deref(), Some("2.0"));
                if self.error.is_none() {
                    self.result = self
                        .result
                        .or_else(|| serde_json::value::to_raw_value(&()).ok());
                } else {
                    assert!(self.result.is_none());
                }
            }
            JsonRpcVersion::Unknown => (),
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tower::{service_fn, ServiceExt};

    use crate::server::cookie;

    const INNER_BODY: &str = "inner service reached";

    /// Sends a `getinfo` request with the given `Authorization` header through a middleware
    /// that requires `cookie`, and returns the response.
    async fn call_with_auth(cookie: Cookie, auth: Option<String>) -> HttpResponse<HttpBody> {
        let inner = service_fn(|_request: HttpRequest| async {
            Ok::<_, BoxError>(HttpResponse::new(HttpBody::from(INNER_BODY)))
        });
        let mut middleware = HttpRequestMiddleware::new(inner, Some(cookie), 1024);

        let mut request = HttpRequest::new(HttpBody::from(
            r#"{"jsonrpc":"2.0","method":"getinfo","params":[],"id":1}"#,
        ));
        if let Some(auth) = auth {
            request.headers_mut().insert(
                header::AUTHORIZATION,
                header::HeaderValue::from_str(&format!("Basic {}", STANDARD.encode(auth))).unwrap(),
            );
        }

        middleware
            .ready()
            .await
            .expect("the middleware is always ready")
            .call(request)
            .await
            .expect("the middleware returns an HTTP response, not a transport error")
    }

    /// Returns the `user:password` credentials `zcash-cli` would read from the cookie file.
    fn cookie_credentials(cookie: &Cookie) -> String {
        let dir = tempfile::tempdir().unwrap();
        cookie::write_to_disk(cookie, dir.path()).unwrap();
        std::fs::read_to_string(dir.path().join(".cookie")).unwrap()
    }

    async fn assert_unauthenticated(response: HttpResponse<HttpBody>) {
        assert_eq!(response.status(), hyper::StatusCode::UNAUTHORIZED);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE),
            Some(&header::HeaderValue::from_static("application/json"))
        );
        assert_eq!(
            response.headers().get(header::WWW_AUTHENTICATE),
            Some(&header::HeaderValue::from_static("Basic realm=\"jsonrpc\""))
        );

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], 401);
        assert_eq!(body["error"]["message"], "unauthenticated method");
    }

    #[test]
    fn static_auth_requires_the_whole_basic_credential() {
        let auth = RpcAuth::static_credentials("node", "one:two");
        let middleware = HttpRequestMiddleware::with_auth((), Some(auth), 1024);
        for (scheme, pair, allowed) in [
            ("bAsIc", "node:one:two", true),
            ("Basic", "node:one", false),
            ("Basic", "peer:one:two", false),
            ("Bearer", "node:one:two", false),
        ] {
            let mut headers = header::HeaderMap::new();
            let value = format!("{scheme} {}", STANDARD.encode(pair));
            headers.insert(header::AUTHORIZATION, value.parse().unwrap());
            assert_eq!(middleware.check_credentials(&headers), allowed);
        }
    }

    #[tokio::test]
    async fn request_without_credentials_gets_a_401_response() {
        assert_unauthenticated(call_with_auth(Cookie::default(), None).await).await;
    }

    #[tokio::test]
    async fn request_with_wrong_password_gets_a_401_response() {
        let cookie = Cookie::default();
        let wrong = cookie_credentials(&Cookie::default());

        assert_unauthenticated(call_with_auth(cookie, Some(wrong)).await).await;
    }

    #[tokio::test]
    async fn request_with_valid_credentials_reaches_inner_service() {
        let cookie = Cookie::default();
        let credentials = cookie_credentials(&cookie);

        let response = call_with_auth(cookie, Some(credentials)).await;

        assert_eq!(response.status(), hyper::StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body.as_ref(), INNER_BODY.as_bytes());
    }
}
