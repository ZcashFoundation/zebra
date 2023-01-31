//! Compatibility fixes for JSON-RPC requests.

use futures::TryStreamExt;
use hyper::{body::Bytes, Body};

use jsonrpc_http_server::RequestMiddleware;

/// HTTP [`RequestMiddleware`] with compatibility workarounds.
///
/// This middleware makes the following changes to requests:
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

        // Fix the request headers
        FixHttpRequestMiddleware::add_missing_content_type_header(request.headers_mut());

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

    /// If the `content-type` HTTP header is not present,
    /// add an `application/json` content type header.
    pub fn add_missing_content_type_header(headers: &mut hyper::header::HeaderMap) {
        headers
            .entry(hyper::header::CONTENT_TYPE)
            .or_insert(hyper::header::HeaderValue::from_static("application/json"));
    }
}
