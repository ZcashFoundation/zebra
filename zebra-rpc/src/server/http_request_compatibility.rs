//! Compatibility fixes for JSON-RPC HTTP requests.
//!
//! These fixes are applied at the HTTP level, before the RPC request is parsed.

use std::{borrow::Cow, future::Future, pin::Pin};

use futures::{future, FutureExt};
use http_body_util::{BodyExt, Full, Limited};
use hyper::header;
use jsonrpsee::{
    core::{server::BoundedWriter, BoxError},
    server::{HttpBody, HttpRequest, HttpResponse},
};
use jsonrpsee_types::{
    error::{
        OVERSIZED_RESPONSE_CODE, OVERSIZED_RESPONSE_MSG, TOO_BIG_BATCH_RESPONSE_CODE,
        TOO_BIG_BATCH_RESPONSE_MSG,
    },
    ErrorObject,
};
use serde::{ser::SerializeMap, Deserialize, Serialize, Serializer};
use serde_json::value::RawValue;
use tower::Service;

use super::cookie::Cookie;

use base64::{engine::general_purpose::STANDARD, Engine as _};

/// HTTP [`HttpRequestMiddleware`] with compatibility workarounds.
///
/// This middleware makes the following changes to HTTP requests:
///
/// ### Normalize legacy JSON-RPC requests and responses
///
/// Upgrades versionless and JSON-RPC 1.0 calls to JSON-RPC 2.0, then restores the
/// client's response format. Homogeneous legacy batches are converted together.
/// Converted bodies respect the configured request and response limits. Response conversion
/// overflow returns a legacy-formatted oversized-response error, retaining jsonrpsee's
/// exception that these error envelopes can exceed the response limit.
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
/// If the cookie-based RPC authentication is enabled, check that the incoming request contains the
/// authentication cookie.
///
/// This enables compatibility with `zcash-cli`.
///
/// ## Security
///
/// Any user-specified data in RPC requests is hex or base58check encoded.
/// We assume lightwalletd validates data encodings before sending it on to Zebra.
/// So any fixes Zebra performs won't change user-specified data.
#[derive(Clone, Debug)]
pub struct HttpRequestMiddleware<S> {
    service: S,
    cookie: Option<Cookie>,
    max_request_body_size: usize,
    max_response_body_size: usize,
}

impl<S> HttpRequestMiddleware<S> {
    /// Create a middleware with the given service, cookie, and request/response body size limits.
    pub fn new(
        service: S,
        cookie: Option<Cookie>,
        max_request_body_size: usize,
        max_response_body_size: usize,
    ) -> Self {
        Self {
            service,
            cookie,
            max_request_body_size,
            max_response_body_size,
        }
    }

    /// Check if the request is authenticated.
    pub fn check_credentials(&self, headers: &header::HeaderMap) -> bool {
        self.cookie.as_ref().is_none_or(|internal_cookie| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|auth_header| auth_header.to_str().ok())
                .and_then(|auth_header| auth_header.split_whitespace().nth(1))
                .and_then(|encoded| STANDARD.decode(encoded).ok())
                .and_then(|decoded| String::from_utf8(decoded).ok())
                .and_then(|request_cookie| request_cookie.split(':').nth(1).map(String::from))
                .is_some_and(|passwd| internal_cookie.authenticate(passwd))
        })
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
    ///
    /// Batches are converted only when all members are legacy request objects with explicit IDs.
    /// Other batches pass through unchanged.
    /// Converted requests must also fit the request body size limit.
    async fn request_to_json_rpc_2(
        request: HttpRequest<HttpBody>,
        max_request_body_size: usize,
    ) -> Result<(JsonRpcVersion, HttpRequest<HttpBody>), BoxError> {
        let (parts, body) = request.into_parts();
        let bytes = Limited::new(body, max_request_body_size)
            .collect()
            .await?
            .to_bytes();
        let (version, rewritten) = if let Ok(request) =
            serde_json::from_slice::<'_, JsonRpcRequest<'_>>(bytes.as_ref())
        {
            let version = request.version();
            if matches!(
                version,
                JsonRpcVersion::Unknown | JsonRpcVersion::TwoPointZero
            ) {
                (version, None)
            } else {
                let mut writer = BoundedWriter::new(max_request_body_size);
                serde_json::to_writer(&mut writer, &request.into_2())?;
                (version, Some(writer.into_bytes()))
            }
        } else if let Some(mut requests) = serde_json::from_slice::<Vec<&RawValue>>(bytes.as_ref())
            .ok()
            .filter(|requests| {
                requests
                    .iter()
                    .all(|request| request.get().trim_start().starts_with('{'))
            })
            .and_then(|requests| {
                requests
                    .into_iter()
                    .map(|request| serde_json::from_str::<JsonRpcRequest<'_>>(request.get()))
                    .collect::<Result<Vec<_>, _>>()
                    .ok()
            })
        {
            let version = requests
                .first()
                .map_or(JsonRpcVersion::Unknown, JsonRpcRequest::version);
            if !matches!(
                version,
                JsonRpcVersion::Bitcoind | JsonRpcVersion::Lightwalletd
            ) || requests.iter().any(|request| request.version() != version)
            {
                (JsonRpcVersion::Unknown, None)
            } else {
                for request in &mut requests {
                    request.jsonrpc = Some(Cow::Borrowed("2.0"));
                }
                let mut writer = BoundedWriter::new(max_request_body_size);
                serde_json::to_writer(&mut writer, &requests)?;
                (version, Some(writer.into_bytes()))
            }
        } else {
            (JsonRpcVersion::Unknown, None)
        };
        let body = match rewritten {
            Some(bytes) => HttpBody::from(bytes),
            None => HttpBody::new(Full::new(bytes)),
        };
        Ok((version, HttpRequest::from_parts(parts, body)))
    }
    /// Maps JSON-2.0 to whatever JSON-RPC version the client is using.
    async fn response_from_json_rpc_2(
        version: JsonRpcVersion,
        response: HttpResponse<HttpBody>,
        max_response_body_size: usize,
    ) -> Result<HttpResponse<HttpBody>, BoxError> {
        if matches!(
            version,
            JsonRpcVersion::Unknown | JsonRpcVersion::TwoPointZero
        ) {
            return Ok(response);
        }

        let (parts, body) = response.into_parts();
        let bytes = body.collect().await?.to_bytes();
        let rewritten = if let Ok(mut response) =
            serde_json::from_slice::<JsonRpcResponse<'_>>(&bytes)
        {
            #[derive(Deserialize)]
            struct ErrorCode {
                code: i32,
            }

            // Preserve upstream oversized errors even when their 2.0 envelope fits the limit.
            // Inspect only the code, keeping the complete error payload borrowed and unchanged.
            // Other already-oversized error envelopes retain jsonrpsee's existing exception.
            let error_limit = bytes.len().saturating_add(r#","result":null"#.len());
            let limit = if error_limit > max_response_body_size
                && response.result.is_none()
                && response.error.is_some_and(|error| {
                    bytes.len() > max_response_body_size
                        || serde_json::from_str::<ErrorCode>(error.get()).is_ok_and(|error| {
                            matches!(
                                error.code,
                                OVERSIZED_RESPONSE_CODE | TOO_BIG_BATCH_RESPONSE_CODE
                            )
                        })
                }) {
                error_limit
            } else {
                max_response_body_size
            };
            response.set_version(version);
            let mut writer = BoundedWriter::new(limit);
            Some(match serde_json::to_writer(&mut writer, &response) {
                Ok(()) => writer.into_bytes(),
                Err(error) if error.is_io() => {
                    drop(writer);
                    Self::oversized_response(version, Some(response.id), max_response_body_size)?
                }
                Err(error) => return Err(error.into()),
            })
        } else if let Ok(mut responses) = serde_json::from_slice::<Vec<JsonRpcResponse<'_>>>(&bytes)
        {
            for response in &mut responses {
                response.set_version(version);
            }
            let mut writer = BoundedWriter::new(max_response_body_size);
            Some(match serde_json::to_writer(&mut writer, &responses) {
                Ok(()) => writer.into_bytes(),
                Err(error) if error.is_io() => {
                    drop(writer);
                    Self::oversized_response(version, None, max_response_body_size)?
                }
                Err(error) => return Err(error.into()),
            })
        } else {
            None
        };
        let body = match rewritten {
            Some(bytes) => HttpBody::from(bytes),
            None => HttpBody::new(Full::new(bytes)),
        };
        Ok(HttpResponse::from_parts(parts, body))
    }

    /// Serialize jsonrpsee's oversized-response error using only fixed framing and the raw ID.
    /// A missing ID denotes an aggregate batch error; an explicit null ID remains a single error.
    fn oversized_response(
        version: JsonRpcVersion,
        id: Option<&RawValue>,
        max_response_body_size: usize,
    ) -> Result<Vec<u8>, BoxError> {
        let (code, message, id) = match id {
            Some(id) => (OVERSIZED_RESPONSE_CODE, OVERSIZED_RESPONSE_MSG, id),
            None => (
                TOO_BIG_BATCH_RESPONSE_CODE,
                TOO_BIG_BATCH_RESPONSE_MSG,
                RawValue::NULL,
            ),
        };
        let data = serde_json::value::to_raw_value(&format!(
            "Exceeded max limit of {max_response_body_size}"
        ))?;
        let error = ErrorObject::borrowed(code, message, Some(data.as_ref()));
        // Both error codes have the same width and the fixed messages need no JSON escaping.
        // Only the raw ID and the bounded diagnostic vary; never retry the original payload
        // with an unlimited writer. Bitcoind's versionless envelope is smaller than this bound.
        let limit =
            r#"{"jsonrpc":"1.0","id":,"result":null,"error":{"code":-32008,"message":"","data":}}"#
                .len()
                .saturating_add(id.get().len())
                .saturating_add(message.len())
                .saturating_add(data.get().len());
        let mut writer = BoundedWriter::new(limit);
        let mut serializer = serde_json::Serializer::new(&mut writer);
        let mut response = serializer.serialize_map(None)?;
        if version == JsonRpcVersion::Lightwalletd {
            response.serialize_entry("jsonrpc", "1.0")?;
        }
        response.serialize_entry("id", id)?;
        response.serialize_entry("result", &())?;
        response.serialize_entry("error", &error)?;
        response.end()?;
        Ok(writer.into_bytes())
    }
}

/// HTTP compatibility layer carrying authentication and request/response body size limits.
#[derive(Clone)]
pub struct HttpRequestMiddlewareLayer {
    cookie: Option<Cookie>,
    max_request_body_size: usize,
    max_response_body_size: usize,
}

impl HttpRequestMiddlewareLayer {
    /// Create a layer with the given cookie and request/response body size limits.
    pub fn new(
        cookie: Option<Cookie>,
        max_request_body_size: usize,
        max_response_body_size: usize,
    ) -> Self {
        Self {
            cookie,
            max_request_body_size,
            max_response_body_size,
        }
    }
}

impl<S> tower::Layer<S> for HttpRequestMiddlewareLayer {
    type Service = HttpRequestMiddleware<S>;

    fn layer(&self, service: S) -> Self::Service {
        HttpRequestMiddleware::new(
            service,
            self.cookie.clone(),
            self.max_request_body_size,
            self.max_response_body_size,
        )
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
            let error = ErrorObject::borrowed(401, "unauthenticated method", None);
            // TODO: Error object is not being returned to the user but an empty response.
            return future::err(BoxError::from(error)).boxed();
        }

        // Fix the request headers.
        Self::insert_or_replace_content_type_header(request.headers_mut());

        let mut service = self.service.clone();
        let max_request_body_size = self.max_request_body_size;
        let max_response_body_size = self.max_response_body_size;

        async move {
            let (version, request) =
                Self::request_to_json_rpc_2(request, max_request_body_size).await?;
            let response = service.call(request).await.map_err(Into::into)?;
            Self::response_from_json_rpc_2(version, response, max_response_body_size).await
        }
        .boxed()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
#[serde_with::serde_as]
#[derive(Debug, Deserialize, Serialize)]
pub(super) struct JsonRpcRequest<'a> {
    #[serde_as(as = "Option<serde_with::BorrowCow>")]
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    jsonrpc: Option<Cow<'a, str>>,
    #[serde(borrow)]
    method: Cow<'a, str>,
    #[serde(borrow, skip_serializing_if = "Option::is_none")]
    params: Option<&'a RawValue>,
    #[serde(
        borrow,
        default,
        deserialize_with = "deserialize_present_id",
        skip_serializing_if = "Option::is_none"
    )]
    id: Option<&'a RawValue>,
}

fn deserialize_present_id<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(deserializer).map(Some)
}

impl JsonRpcRequest<'_> {
    fn version(&self) -> JsonRpcVersion {
        match (self.jsonrpc.as_deref(), self.params, self.id) {
            // RawValue validates JSON; its first byte distinguishes the allowed scalar IDs.
            (Some("2.0"), _, id)
                if id.is_none_or(|id| {
                    matches!(id.get().as_bytes()[0], b'n' | b'"' | b'-' | b'0'..=b'9')
                }) =>
            {
                JsonRpcVersion::TwoPointZero
            }
            (Some("1.0"), Some(_), Some(_)) => JsonRpcVersion::Lightwalletd,
            (None, Some(_), Some(_)) => JsonRpcVersion::Bitcoind,
            _ => JsonRpcVersion::Unknown,
        }
    }

    pub(super) fn into_2(mut self) -> Self {
        self.jsonrpc = Some(Cow::Borrowed("2.0"));
        self
    }
}
/// A version-agnostic JSON-RPC response.
#[serde_with::serde_as]
#[derive(Debug, Deserialize, Serialize)]
struct JsonRpcResponse<'a> {
    #[serde_as(as = "Option<serde_with::BorrowCow>")]
    #[serde(default, borrow, skip_serializing_if = "Option::is_none")]
    jsonrpc: Option<Cow<'a, str>>,
    #[serde(borrow)]
    id: &'a RawValue,
    #[serde(borrow, skip_serializing_if = "Option::is_none")]
    result: Option<&'a RawValue>,
    #[serde(borrow, skip_serializing_if = "Option::is_none")]
    error: Option<&'a RawValue>,
}

impl JsonRpcResponse<'_> {
    fn set_version(&mut self, version: JsonRpcVersion) {
        match version {
            JsonRpcVersion::Bitcoind => self.jsonrpc = None,
            JsonRpcVersion::Lightwalletd => self.jsonrpc = Some(Cow::Borrowed("1.0")),
            JsonRpcVersion::TwoPointZero | JsonRpcVersion::Unknown => return,
        }
        self.result = Some(self.result.unwrap_or(RawValue::NULL));
        self.error = Some(self.error.unwrap_or(RawValue::NULL));
    }
}
