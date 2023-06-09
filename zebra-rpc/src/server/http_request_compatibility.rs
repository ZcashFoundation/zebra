//! Compatibility fixes for JSON-RPC HTTP requests.
//!
//! These fixes are applied at the HTTP level, before the RPC request is parsed.

use futures::TryStreamExt;
use hyper::{body::Bytes, Body};

use jsonrpc_http_server::RequestMiddleware;

/// HTTP [`RequestMiddleware`] with compatibility workarounds.
///
/// This middleware makes the following changes to HTTP requests:
///
/// ## Remove `jsonrpc` field in JSON RPC 1.0
///
/// Removes "jsonrpc: 1.0" fields from requests,
/// because the "jsonrpc" field was only added in JSON-RPC 2.0.
///
/// <http://www.simple-is-better.org/rpc/#differences-between-1-0-and-2-0>
///
/// ## Add missing `content-type` HTTP header
///
/// Some RPC clients don't include a `content-type` HTTP header.
/// But unlike web browsers, [`jsonrpc_http_server`] does not do content sniffing.
///
/// If there is no `content-type` header, we assume the content is JSON,
/// and let the parser error if we are incorrect.
///
/// This enables compatibility with `zcash-cli`.
///
/// ## Security
///
/// Any user-specified data in RPC requests is hex or base58check encoded.
/// We assume lightwalletd validates data encodings before sending it on to Zebra.
/// So any fixes Zebra performs won't change user-specified data.
#[derive(Copy, Clone, Debug)]
pub struct FixHttpRequestMiddleware;

impl RequestMiddleware for FixHttpRequestMiddleware {
    fn on_request(
        &self,
        mut request: hyper::Request<hyper::Body>,
    ) -> jsonrpc_http_server::RequestMiddlewareAction {
        tracing::trace!(?request, "original HTTP request");

        // Fix the request headers if needed and we can do so.
        FixHttpRequestMiddleware::insert_or_replace_content_type_header(request.headers_mut());

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

        jsonrpc_http_server::RequestMiddlewareAction::Proceed {
            // TODO: disable this security check if we see errors from lightwalletd.
            should_continue_on_invalid_cors: false,
            request,
        }
    }
}

impl FixHttpRequestMiddleware {
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
    /// - supplied `content-type` is `text/plain`.
    ///
    /// `application/json` is the only `content-type` accepted by the Zebra rpc endpoint:
    ///
    /// <https://github.com/paritytech/jsonrpc/blob/38af3c9439aa75481805edf6c05c6622a5ab1e70/http/src/handler.rs#L582-L584>
    pub fn insert_or_replace_content_type_header(headers: &mut hyper::header::HeaderMap) {
        match headers.entry(hyper::header::CONTENT_TYPE) {
            hyper::header::Entry::Vacant(_) => {
                headers.insert(
                    hyper::header::CONTENT_TYPE,
                    hyper::header::HeaderValue::from_static("application/json"),
                );
            }
            hyper::header::Entry::Occupied(header_data) => {
                if header_data
                    .iter()
                    .filter_map(|x| x.to_str().ok())
                    .map(|s| s == "text/plain")
                    .next()
                    .unwrap_or(false)
                {
                    headers.insert(
                        hyper::header::CONTENT_TYPE,
                        hyper::header::HeaderValue::from_static("application/json"),
                    );
                }
            }
        };
    }
}
