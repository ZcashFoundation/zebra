//! Compatibility fixes for JSON-RPC HTTP requests.
//!
//! These fixes are applied at the HTTP level, before the RPC request is parsed.

use std::{borrow::Cow, future::Future, pin::Pin};

use bytes::Bytes;
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
/// Requests that are already canonical JSON-RPC 2.0, and their responses, are forwarded unchanged.
/// Responses to 1.0 requests are converted by rewriting only their head and tail, keeping `result` as is.
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

    /// Returns the HTTP 401 response sent when the request has no valid cookie credentials.
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

        // Re-encoding these requests would produce the same JSON-RPC 2.0 call, so skip it.
        if is_canonical_json_rpc_2_request(&bytes) {
            return Ok((
                JsonRpcVersion::TwoPointZero,
                HttpRequest::from_parts(parts, bytes_body(bytes)),
            ));
        }

        let (version, rewritten) = if let Ok(mut request) =
            serde_json::from_slice::<'_, JsonRpcRequest<'_>>(bytes.as_ref())
        {
            let version = request.version();
            // Other 2.0 calls are re-encoded with decoded params, so `jsonrpsee` reads them as it
            // did before the fast path. Legacy calls keep their raw params.
            let decoded_params = match (version, request.params) {
                (JsonRpcVersion::TwoPointZero, Some(params)) => {
                    serde_json::from_str::<serde_json::Value>(params.get())
                        .ok()
                        .map(|params| {
                            serde_json::value::to_raw_value(&params)
                                .expect("decoded JSON values always re-encode")
                        })
                }
                _ => None,
            };
            if decoded_params.is_some() {
                request.params = decoded_params.as_deref();
            }
            if matches!(version, JsonRpcVersion::Unknown) {
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
            None => bytes_body(bytes),
        };
        Ok((version, HttpRequest::from_parts(parts, body)))
    }

    /// Maps JSON-2.0 to whatever JSON-RPC version the client is using.
    async fn response_from_json_rpc_2(
        version: JsonRpcVersion,
        response: HttpResponse<HttpBody>,
        max_response_body_size: usize,
    ) -> Result<HttpResponse<HttpBody>, BoxError> {
        // 2.0 clients get the server's response as is, and unparsed requests have nothing to map.
        if matches!(
            version,
            JsonRpcVersion::TwoPointZero | JsonRpcVersion::Unknown
        ) {
            return Ok(response);
        }

        let (parts, body) = response.into_parts();
        let bytes = body.collect().await?.to_bytes();
        let bytes = response_to_version(bytes, version, max_response_body_size)?;
        Ok(HttpResponse::from_parts(parts, bytes_body(bytes)))
    }
}

/// Converts a JSON-RPC 2.0 response body to `version`, splicing it when it has the
/// `jsonrpsee` layout and fits the response limit, and parsing it otherwise.
pub(super) fn response_to_version(
    bytes: Bytes,
    version: JsonRpcVersion,
    max_response_body_size: usize,
) -> Result<Bytes, BoxError> {
    match splice_response(&bytes, version) {
        Some(spliced) if spliced.len() <= max_response_body_size => Ok(spliced),
        _ => reencode_response(bytes, version, max_response_body_size),
    }
}

/// Parses a response or batch body and re-encodes it as `version` within the response limit,
/// or returns it unchanged if it doesn't parse.
pub(super) fn reencode_response(
    bytes: Bytes,
    version: JsonRpcVersion,
    max_response_body_size: usize,
) -> Result<Bytes, BoxError> {
    let rewritten = if let Ok(mut response) = serde_json::from_slice::<JsonRpcResponse<'_>>(&bytes)
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
                oversized_response(version, Some(response.id), max_response_body_size)?
            }
            Err(error) => return Err(error.into()),
        })
    } else if let Ok(mut responses) = serde_json::from_slice::<Vec<JsonRpcResponse<'_>>>(&bytes) {
        for response in &mut responses {
            response.set_version(version);
        }
        let mut writer = BoundedWriter::new(max_response_body_size);
        Some(match serde_json::to_writer(&mut writer, &responses) {
            Ok(()) => writer.into_bytes(),
            Err(error) if error.is_io() => {
                drop(writer);
                oversized_response(version, None, max_response_body_size)?
            }
            Err(error) => return Err(error.into()),
        })
    } else {
        None
    };
    Ok(rewritten.map_or(bytes, Bytes::from))
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

/// Converts a `jsonrpsee` response, `{"jsonrpc":"2.0","id":…,"result":…}` or `…"error":…}`,
/// to a 1.0 `version` by rewriting its head and tail. Returns `None` for any other layout.
pub(super) fn splice_response(body: &[u8], version: JsonRpcVersion) -> Option<Bytes> {
    let head: &[u8] = match version {
        JsonRpcVersion::Lightwalletd => br#"{"jsonrpc":"1.0","id":"#,
        JsonRpcVersion::Bitcoind => br#"{"id":"#,
        JsonRpcVersion::TwoPointZero | JsonRpcVersion::Unknown => return None,
    };
    let rest = body.strip_prefix(br#"{"jsonrpc":"2.0","id":"#)?;
    let (id, rest) = rest.split_at(scalar_len(rest)?);
    let rest = rest.strip_suffix(b"}")?;

    // The id and error are small next to the result, so check them as the parsing path does,
    // which keeps them as sent.
    if !is_one_raw_value(id) {
        return None;
    }
    if let Some(result) = rest.strip_prefix(br#","result":"#) {
        return is_one_value(result).then(|| splice(head, id, result, b"null"));
    }
    let error = rest.strip_prefix(br#","error":"#)?;
    is_one_raw_value(error).then(|| splice(head, id, b"null", error))
}

/// Joins the parts of a 1.0 response, which has both `result` and `error`, as zcashd sends.
fn splice(head: &[u8], id: &[u8], result: &[u8], error: &[u8]) -> Bytes {
    let parts: [&[u8]; 7] = [
        head,
        id,
        br#","result":"#,
        result,
        br#","error":"#,
        error,
        b"}",
    ];
    parts.concat().into()
}

/// Returns the length of the string, number or `null` at the start of `head`, if it is followed
/// by a comma in `head`. Strings end at their closing quote, other values at the comma.
fn scalar_len(head: &[u8]) -> Option<usize> {
    if head.first() != Some(&b'"') {
        return memchr::memchr(b',', head);
    }
    let mut end = 1;
    loop {
        end += memchr::memchr2(b'"', b'\\', head.get(end..)?)?;
        if head[end] == b'"' {
            return Some(end + 1);
        }
        end += 2;
    }
}

/// Returns true if `value` is exactly one JSON value, without surrounding whitespace.
fn is_one_raw_value(value: &[u8]) -> bool {
    serde_json::from_slice::<&RawValue>(value).is_ok_and(|raw| raw.get().len() == value.len())
}

/// Returns true if `value` looks like one JSON value from its first and last bytes.
/// Scalars are short, so they are parsed; containers and strings are trusted to `jsonrpsee`.
fn is_one_value(value: &[u8]) -> bool {
    match (value.first(), value.last()) {
        (Some(b'{'), Some(b'}')) | (Some(b'['), Some(b']')) => true,
        (Some(b'"'), Some(b'"')) => value.len() >= 2,
        (Some(b'{' | b'[' | b'"'), _) => false,
        (Some(first), Some(last)) => {
            !first.is_ascii_whitespace()
                && !last.is_ascii_whitespace()
                && serde_json::from_slice::<serde::de::IgnoredAny>(value).is_ok()
        }
        _ => false,
    }
}

/// Wraps `bytes` in a body without copying them.
fn bytes_body(bytes: Bytes) -> HttpBody {
    HttpBody::new(Full::new(bytes))
}

/// Returns true if `body` is a single JSON-RPC 2.0 call that `jsonrpsee` reads the same way
/// before and after re-encoding: only known fields, no escapes, canonical positional params.
pub(super) fn is_canonical_json_rpc_2_request(body: &[u8]) -> bool {
    is_canonical_submitblock_request(body) || is_canonical_json_rpc_2_request_slow(body)
}

pub(super) fn is_canonical_json_rpc_2_request_slow(body: &[u8]) -> bool {
    // `jsonrpsee` only skips 128 bytes of leading whitespace, and re-encoding removes it.
    if body.first() != Some(&b'{') {
        return false;
    }
    let Ok(request) = serde_json::from_slice::<RawJsonRpcRequest>(body) else {
        return false;
    };

    request.jsonrpc.get() == r#""2.0""#
        && is_unescaped_string(request.method)
        && request
            .id
            .is_none_or(|id| is_unescaped_string(id) || id.get().parse::<u64>().is_ok())
        && request.params.is_none_or(is_canonical_params)
}

/// A strict subset of canonical requests that needs only one SIMD scan before jsonrpsee parses it.
pub(super) fn is_canonical_submitblock_request(body: &[u8]) -> bool {
    // Adapted from Zakura (MIT OR Apache-2.0): avoid parsing a large unescaped hex string twice.
    const PREFIX: &[u8] = br#"{"jsonrpc":"2.0","method":"submitblock","params":[""#;
    let Some(rest) = body.strip_prefix(PREFIX) else {
        return false;
    };
    let Some(end) = memchr::memmem::rfind(rest, br#""],"id":"#) else {
        return false;
    };
    let (hex, tail) = rest.split_at(end);
    let Some(id) = tail
        .strip_prefix(br#""],"id":"#)
        .and_then(|tail| tail.strip_suffix(b"}"))
    else {
        return false;
    };
    let Ok(id) = std::str::from_utf8(id) else {
        return false;
    };
    id.len() <= 20
        && (id == "0" || !id.starts_with('0'))
        && id.bytes().all(|byte| byte.is_ascii_digit())
        && id.parse::<u64>().is_ok()
        && const_hex::check_raw(hex)
}

/// Returns true if `params` is an unspaced array of unescaped strings, integers, booleans or nulls.
/// Re-encoding changes floats, objects and escapes, and `jsonrpsee` rejects `[ ]` but not `[]`.
fn is_canonical_params(params: &RawValue) -> bool {
    // `params` is valid JSON, so splitting it at the top level only needs to find string ends.
    let Some(mut rest) = params.get().as_bytes().strip_prefix(b"[") else {
        return false;
    };
    if rest == b"]" {
        return true;
    }
    loop {
        let end = if let Some(string) = rest.strip_prefix(b"\"") {
            match memchr::memchr2(b'"', b'\\', string) {
                Some(end) if string[end] == b'"' => end + 2,
                _ => return false,
            }
        } else {
            let end = memchr::memchr2(b',', b']', rest).unwrap_or(rest.len());
            if !is_canonical_scalar(&rest[..end]) {
                return false;
            }
            end
        };
        match rest.get(end..) {
            Some(b"]") => return true,
            Some([b',', next @ ..]) => rest = next,
            _ => return false,
        }
    }
}

/// Returns true if `value` is `true`, `false`, `null` or an integer that re-encodes to itself.
fn is_canonical_scalar(value: &[u8]) -> bool {
    let Ok(value) = std::str::from_utf8(value) else {
        return false;
    };
    match value {
        "true" | "false" | "null" => true,
        // serde_json reads `-0` as a float.
        "-0" => false,
        _ if value.starts_with('-') => value.parse::<i64>().is_ok(),
        _ => value.parse::<u64>().is_ok(),
    }
}

/// Returns true if `value` is a JSON string without escapes.
fn is_unescaped_string(value: &RawValue) -> bool {
    let value = value.get().as_bytes();
    value.first() == Some(&b'"') && memchr::memchr(b'\\', value).is_none()
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
            return future::ok(Self::unauthenticated_response()).boxed();
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
pub(super) enum JsonRpcVersion {
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

/// A JSON-RPC request borrowed from the body, with its values left undecoded.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawJsonRpcRequest<'a> {
    #[serde(borrow)]
    jsonrpc: &'a RawValue,
    #[serde(borrow)]
    method: &'a RawValue,
    #[serde(default, borrow)]
    params: Option<&'a RawValue>,
    #[serde(default, borrow)]
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
        let mut middleware = HttpRequestMiddleware::new(inner, Some(cookie), 1024, 1024);

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
    fn canonical_params_are_only_those_re_encoding_keeps() {
        let canonical = |params: &str| {
            is_canonical_params(&RawValue::from_string(params.into()).expect("valid json"))
        };
        for params in [
            r#"[]"#,
            r#"["ab"]"#,
            r#"["",0,-1,true,false,null]"#,
            r#"["a,]",1]"#,
        ] {
            assert!(canonical(params), "{params}");
        }
        for params in [
            r#"[ ]"#,
            r#"[1 ]"#,
            r#"["a", 1]"#,
            r#"["\n"]"#,
            r#"[-0]"#,
            r#"[1.0]"#,
            r#"[1e2]"#,
            r#"[18446744073709551616]"#,
            r#"[[1]]"#,
            r#"[{"a":1}]"#,
            r#"{"a":1}"#,
            r#"null"#,
        ] {
            assert!(!canonical(params), "{params}");
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
