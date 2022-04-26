//! A JSON-RPC 1.0 & 2.0 endpoint for Zebra.
//!
//! This endpoint is compatible with clients that incorrectly send
//! `"jsonrpc" = 1.0` fields in JSON-RPC 1.0 requests,
//! such as `lightwalletd`.
//!
//! See the full list of
//! [Differences between JSON-RPC 1.0 and 2.0.](https://www.simple-is-better.org/rpc/#differences-between-1-0-and-2-0)

use jsonrpc_core::{Compatibility, MetaIoHandler};
use jsonrpc_http_server::ServerBuilder;
use tokio::task::JoinHandle;
use tower::{buffer::Buffer, Service};
use tracing::*;
use tracing_futures::Instrument;

use zebra_chain::{chain_tip::ChainTip, parameters::Network};
use zebra_node_services::{mempool, BoxError};

use crate::{
    config::Config,
    methods::{Rpc, RpcImpl},
    server::{compatibility::FixHttpRequestMiddleware, tracing_middleware::TracingMiddleware},
};

pub mod compatibility;
mod tracing_middleware;

/// Zebra RPC Server
#[derive(Clone, Debug)]
pub struct RpcServer;

impl RpcServer {
    /// Start a new RPC server endpoint
    pub fn spawn<Version, Mempool, State, Tip>(
        config: Config,
        app_version: Version,
        mempool: Buffer<Mempool, mempool::Request>,
        state: State,
        latest_chain_tip: Tip,
        network: Network,
    ) -> (JoinHandle<()>, JoinHandle<()>)
    where
        Version: ToString,
        Mempool: tower::Service<mempool::Request, Response = mempool::Response, Error = BoxError>
            + 'static,
        Mempool::Future: Send,
        State: Service<
                zebra_state::ReadRequest,
                Response = zebra_state::ReadResponse,
                Error = zebra_state::BoxError,
            > + Clone
            + Send
            + Sync
            + 'static,
        State::Future: Send,
        Tip: ChainTip + Clone + Send + Sync + 'static,
    {
        if let Some(listen_addr) = config.listen_addr {
            info!("Trying to open RPC endpoint at {}...", listen_addr,);

            // Initialize the rpc methods with the zebra version
            let (rpc_impl, rpc_tx_queue_task_handle) =
                RpcImpl::new(app_version, mempool, state, latest_chain_tip, network);

            // Create handler compatible with V1 and V2 RPC protocols
            let mut io: MetaIoHandler<(), _> =
                MetaIoHandler::new(Compatibility::Both, TracingMiddleware);
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

            (
                tokio::task::spawn_blocking(server),
                rpc_tx_queue_task_handle,
            )
        } else {
            // There is no RPC port, so the RPC tasks do nothing.
            (
                tokio::task::spawn(futures::future::pending().in_current_span()),
                tokio::task::spawn(futures::future::pending().in_current_span()),
            )
        }
    }
}
