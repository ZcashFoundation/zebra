//! A JSON-RPC 1.0 & 2.0 endpoint for Zebra.
//!
//! This endpoint is compatible with clients that incorrectly send
//! `"jsonrpc" = 1.0` fields in JSON-RPC 1.0 requests,
//! such as `lightwalletd`.

use tower::{buffer::Buffer, timeout::Timeout, util::BoxService, Service, ServiceExt};
use tracing::*;
use tracing_futures::Instrument;

use jsonrpc_core;
use jsonrpc_http_server::ServerBuilder;

use crate::{
    config::Config,
    methods::{Rpc, RpcImpl},
    server::compatibility::FixHttpRequestMiddleware,
};

pub mod compatibility;

type State = Buffer<
    BoxService<zebra_state::Request, zebra_state::Response, zebra_state::BoxError>,
    zebra_state::Request,
>;

/// Zebra RPC Server
#[derive(Clone, Debug)]
pub struct RpcServer;

impl RpcServer {
    /// Start a new RPC server endpoint
    pub fn spawn(
        config: Config,
        app_version: String,
        state_service: State,
    ) -> tokio::task::JoinHandle<()> {
        if let Some(listen_addr) = config.listen_addr {
            info!("Trying to open RPC endpoint at {}...", listen_addr,);

            // Initialize the rpc methods with the zebra version
            let rpc_impl = RpcImpl {
                app_version,
                state_service,
            };

            // Create handler compatible with V1 and V2 RPC protocols
            let mut io =
                jsonrpc_core::IoHandler::with_compatibility(jsonrpc_core::Compatibility::Both);
            io.extend_with(rpc_impl.to_delegate());

            let server = ServerBuilder::new(io)
                // use the same tokio executor as the rest of Zebra
                .event_loop_executor(tokio::runtime::Handle::current())
                .threads(1)
                // TODO: disable this security check if we see errors from lightwalletd.
                //.allowed_hosts(DomainsValidation::Disabled)
                .request_middleware(FixHttpRequestMiddleware)
                .start_http(&listen_addr)
                .expect("Unable to start RPC server");

            // The server is a blocking task, so we need to spawn it on a blocking thread.
            let span = Span::current();
            let server = move || {
                span.in_scope(|| {
                    info!("Opened RPC endpoint at {}", server.address());

                    server.wait();

                    info!("Stopping RPC endpoint");
                })
            };

            tokio::task::spawn_blocking(server)
        } else {
            // There is no RPC port, so the RPC task does nothing.
            tokio::task::spawn(futures::future::pending().in_current_span())
        }
    }
}
