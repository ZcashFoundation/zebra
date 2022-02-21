//! An RPC endpoint.

use futures::TryStreamExt;
use hyper::{body::Bytes, Body};
use jsonrpc_core;
use jsonrpc_http_server::{RequestMiddleware, ServerBuilder};

use zebra_rpc::rpc::{Rpc, RpcImpl};

pub mod config;
pub use config::Config;

/// Zebra RPC Server
pub struct RpcServer {}

impl RpcServer {
    /// Start a new RPC server endpoint
    pub async fn new(config: Config) -> Self {
        if config.listen_addr.is_some() {
            info!(
                "Trying to open RPC endpoint at {}...",
                config.listen_addr.unwrap()
            );

            // Create handler compatible with V1 and V2 RPC protocols
            let mut io =
                jsonrpc_core::IoHandler::with_compatibility(jsonrpc_core::Compatibility::Both);
            io.extend_with(RpcImpl.to_delegate());

            let server = ServerBuilder::new(io)
                // use the same tokio executor as the rest of Zebra
                .event_loop_executor(tokio::runtime::Handle::current())
                .threads(1)
                // TODO: disable this security check if we see errors from lightwalletd.
                //.allowed_hosts(DomainsValidation::Disabled)
                .request_middleware(FixHttpRequestMiddleware)
                .start_http(&config.listen_addr.unwrap())
                .expect("Unable to start RPC server");

            info!("Opened RPC endpoint at {}", server.address());

            server.wait();
        }
        RpcServer {}
    }
}

/// HTTP [`RequestMiddleware`] with compatibility wrokarounds.
///
/// This middleware makes the following changes to requests:
///
/// ## JSON RPC 1.0 `jsonrpc` field
///
/// Removes "jsonrpc: 1.0" fields from requests,
/// because the "jsonrpc" field was only added in JSON-RPC 2.0.
///
/// <http://www.simple-is-better.org/rpc/#differences-between-1-0-and-2-0>
//
// TODO: put this HTTP middleware in a separate module
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FixHttpRequestMiddleware;

impl RequestMiddleware for FixHttpRequestMiddleware {
    fn on_request(
        &self,
        request: hyper::Request<hyper::Body>,
    ) -> jsonrpc_http_server::RequestMiddlewareAction {
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
        // TODO: replace this with a regular expression if we see errors from lightwalletd.
        data.replace("\"jsonrpc\":\"1.0\",", "")
            .replace("\"jsonrpc\": \"1.0\",", "")
            .replace(",\"jsonrpc\":\"1.0\"", "")
            .replace(", \"jsonrpc\": \"1.0\"", "")
    }
}
