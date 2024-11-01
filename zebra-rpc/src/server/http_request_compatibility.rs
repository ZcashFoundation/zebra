//! Compatibility fixes for JSON-RPC HTTP requests.
//!
//! These fixes are applied at the HTTP level, before the RPC request is parsed.

use base64::{engine::general_purpose::URL_SAFE, Engine as _};
use futures::TryStreamExt;
use jsonrpc_http_server::{
    hyper::{body::Bytes, header, Body, Request},
    RequestMiddleware, RequestMiddlewareAction,
};

use crate::server::{
    cookie::Cookie,
    jsonrpc::{JsonRpcError, JsonRpcRequest, JsonRpcResponse},
    Empty, EndpointClient, Request as TonicRequest,
};

/// HTTP [`RequestMiddleware`] with compatibility workarounds.
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
/// But unlike web browsers, [`jsonrpc_http_server`] does not do content sniffing.
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
#[derive(Clone, Debug, Default)]
pub struct HttpRequestMiddleware {
    cookie: Option<Cookie>,
}

/// A trait for updating an object, consuming it and returning the updated version.
pub trait With<T> {
    /// Updates `self` with an instance of type `T` and returns the updated version of `self`.
    fn with(self, _: T) -> Self;
}

impl With<Cookie> for HttpRequestMiddleware {
    fn with(mut self, cookie: Cookie) -> Self {
        self.cookie = Some(cookie);
        self
    }
}

impl HttpRequestMiddleware {
    /// Check if the number of parameters matches the expected length.
    pub fn check_parameters_length(
        request: JsonRpcRequest,
        expeted_len: usize,
    ) -> Option<JsonRpcResponse> {
        if request.params().len() != expeted_len {
            return Some(JsonRpcResponse::new(
                serde_json::to_value("invalid number of parameters")
                    .expect("string to serde_json::Value conversion"),
                request.id().to_string(),
            ));
        }
        None
    }

    /// Handle incoming JSON-RPC requests
    pub async fn handle_request(
        &self,
        request: JsonRpcRequest,
        headers: warp::http::HeaderMap,
        mut grpc_client: EndpointClient<tonic::transport::Channel>,
    ) -> Result<impl warp::Reply, warp::Rejection> {
        tracing::trace!(?request, "original HTTP request");

        // Check if the request is authenticated
        if !self.check_credentials(&headers) {
            let error = JsonRpcError {
                code: 401,
                message: "unauthenticated method".to_string(),
            };

            let response = JsonRpcResponse::new(
                serde_json::to_value(error).unwrap(),
                request.id().to_string(),
            );

            return Ok(warp::reply::json(&response));
        }

        // Match the JSON-RPC `method` field to call the appropriate gRPC method
        match request.method() {
            "getinfo" => {
                // Check for exactly zero parameter
                if let Some(response) = Self::check_parameters_length(request.clone(), 0) {
                    return Ok(warp::reply::json(&response));
                }

                let grpc_request = TonicRequest::new(Empty {});
                let grpc_response = grpc_client
                    .get_info(grpc_request)
                    .await
                    .map(|grpc_response| serde_json::to_value(grpc_response.into_inner()))
                    .unwrap_or_else(|e| serde_json::to_value(e.to_string()))
                    .map_err(|_| warp::reject::reject())?;
                let json_response = JsonRpcResponse::new(grpc_response, request.id().to_string());

                Ok(warp::reply::json(&json_response))
            }
            "getblockchaininfo" => {
                // Check for exactly zero parameter
                if let Some(response) = Self::check_parameters_length(request.clone(), 0) {
                    return Ok(warp::reply::json(&response));
                }

                let grpc_request = TonicRequest::new(Empty {});
                let grpc_response = grpc_client
                    .get_blockchain_info(grpc_request)
                    .await
                    .map(|grpc_response| serde_json::to_value(grpc_response.into_inner()))
                    .unwrap_or_else(|e| serde_json::to_value(e.to_string()))
                    .map_err(|_| warp::reject::reject())?;
                let json_response = JsonRpcResponse::new(grpc_response, request.id().to_string());

                Ok(warp::reply::json(&json_response))
            }
            "getaddressbalance" => {
                // Check for exactly one parameter
                if let Some(response) = Self::check_parameters_length(request.clone(), 1) {
                    return Ok(warp::reply::json(&response));
                }

                let address_params: crate::server::AddressStrings =
                    serde_json::from_value(request.params().get(0).cloned().unwrap_or_default())
                        .map_err(|_| warp::reject::reject())?;

                let grpc_response = grpc_client
                    .get_address_balance(address_params)
                    .await
                    .map(|grpc_response| serde_json::to_value(grpc_response.into_inner()))
                    .unwrap_or_else(|e| serde_json::to_value(e.to_string()))
                    .map_err(|_| warp::reject::reject())?;

                let json_response = JsonRpcResponse::new(grpc_response, request.id().to_string());

                Ok(warp::reply::json(&json_response))
            }
            "sendrawtransaction" => {
                // Check for exactly one parameter
                if let Some(response) = Self::check_parameters_length(request.clone(), 1) {
                    return Ok(warp::reply::json(&response));
                }

                let hex_param = request.params()[0].as_str().ok_or_else(|| warp::reject())?;
                let grpc_request = TonicRequest::new(crate::server::RawTransactionHex {
                    hex: hex_param.to_string(),
                });

                let grpc_response = grpc_client
                    .send_raw_transaction(grpc_request)
                    .await
                    .map(|grpc_response| serde_json::to_value(grpc_response.into_inner()))
                    .unwrap_or_else(|e| serde_json::to_value(e.to_string()))
                    .map_err(|_| warp::reject::reject())?;

                let json_response = JsonRpcResponse::new(grpc_response, request.id().to_string());

                Ok(warp::reply::json(&json_response))
            }
            _ => {
                let json_response = JsonRpcResponse::new(
                    serde_json::to_value("unsupported method")
                        .expect("string to serde_json::Value conversion"),
                    request.id().to_string(),
                );

                Ok(warp::reply::json(&json_response))
            }
        }
    }
}

impl RequestMiddleware for HttpRequestMiddleware {
    fn on_request(&self, mut request: Request<Body>) -> RequestMiddlewareAction {
        tracing::trace!(?request, "original HTTP request");

        // Check if the request is authenticated
        if !self.check_credentials(request.headers_mut()) {
            let error = jsonrpc_core::Error {
                code: jsonrpc_core::ErrorCode::ServerError(401),
                message: "unauthenticated method".to_string(),
                data: None,
            };
            return jsonrpc_http_server::Response {
                code: jsonrpc_http_server::hyper::StatusCode::from_u16(401)
                    .expect("hard-coded status code should be valid"),
                content_type: header::HeaderValue::from_static("application/json; charset=utf-8"),
                content: serde_json::to_string(&jsonrpc_core::Response::from(error, None))
                    .expect("hard-coded result should serialize"),
            }
            .into();
        }

        // Fix the request headers if needed and we can do so.
        HttpRequestMiddleware::insert_or_replace_content_type_header(request.headers_mut());

        // Fix the request body
        let request = request.map(|body| {
            let body = body.map_ok(|data| {
                // To simplify data handling, we assume that any search strings won't be split
                // across multiple `Bytes` data buffers.
                //
                // To simplify error handling, Zebra only supports valid UTF-8 requests,
                // and uses lossy UTF-8 conversion.
                //
                // JSON-RPC requires all requests to be valid UTF-8.
                // The lower layers should reject invalid requests with lossy changes.
                // But if they accept some lossy changes, that's ok,
                // because the request was non-standard anyway.
                //
                // We're not concerned about performance here, so we just clone the Cow<str>
                let data = String::from_utf8_lossy(data.as_ref()).to_string();

                // Fix up the request.
                let data = Self::remove_json_1_fields(data);

                Bytes::from(data)
            });

            Body::wrap_stream(body)
        });

        tracing::trace!(?request, "modified HTTP request");

        RequestMiddlewareAction::Proceed {
            // TODO: disable this security check if we see errors from lightwalletd.
            should_continue_on_invalid_cors: false,
            request,
        }
    }
}

impl HttpRequestMiddleware {
    /// Remove any "jsonrpc: 1.0" fields in `data`, and return the resulting string.
    pub fn remove_json_1_fields(data: String) -> String {
        // Replace "jsonrpc = 1.0":
        // - at the start or middle of a list, and
        // - at the end of a list;
        // with no spaces (lightwalletd format), and spaces after separators (example format).
        //
        // TODO: if we see errors from lightwalletd, make this replacement more accurate:
        //     - use a partial JSON fragment parser
        //     - combine the whole request into a single buffer, and use a JSON parser
        //     - use a regular expression
        //
        // We could also just handle the exact lightwalletd format,
        // by replacing `{"jsonrpc":"1.0",` with `{`.
        data.replace("\"jsonrpc\":\"1.0\",", "")
            .replace("\"jsonrpc\": \"1.0\",", "")
            .replace(",\"jsonrpc\":\"1.0\"", "")
            .replace(", \"jsonrpc\": \"1.0\"", "")
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

    /// Check if the request is authenticated.
    pub fn check_credentials(&self, headers: &header::HeaderMap) -> bool {
        self.cookie.as_ref().map_or(true, |internal_cookie| {
            headers
                .get(header::AUTHORIZATION)
                .and_then(|auth_header| auth_header.to_str().ok())
                .and_then(|auth_header| auth_header.split_whitespace().nth(1))
                .and_then(|encoded| URL_SAFE.decode(encoded).ok())
                .and_then(|decoded| String::from_utf8(decoded).ok())
                .and_then(|request_cookie| request_cookie.split(':').nth(1).map(String::from))
                .map_or(false, |passwd| internal_cookie.authenticate(passwd))
        })
    }
}
