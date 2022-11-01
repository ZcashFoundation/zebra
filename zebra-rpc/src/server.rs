//! A JSON-RPC 1.0 & 2.0 endpoint for Zebra.
//!
//! This endpoint is compatible with clients that incorrectly send
//! `"jsonrpc" = 1.0` fields in JSON-RPC 1.0 requests,
//! such as `lightwalletd`.
//!
//! See the full list of
//! [Differences between JSON-RPC 1.0 and 2.0.](https://www.simple-is-better.org/rpc/#differences-between-1-0-and-2-0)

use std::panic;

use jsonrpc_core::{Compatibility, MetaIoHandler};
use jsonrpc_http_server::ServerBuilder;
use tokio::task::JoinHandle;
use tower::{buffer::Buffer, Service};

#[cfg(feature = "getblocktemplate-rpcs")]
use tower::{util::BoxService, ServiceBuilder};

use tracing::*;
use tracing_futures::Instrument;

use zebra_chain::{chain_tip::ChainTip, parameters::Network};
use zebra_node_services::{mempool, BoxError};

#[cfg(feature = "getblocktemplate-rpcs")]
use zebra_consensus::{error::TransactionError, transaction, BlockVerifier};

use crate::{
    config::Config,
    methods::{Rpc, RpcImpl},
    server::{compatibility::FixHttpRequestMiddleware, tracing_middleware::TracingMiddleware},
};

#[cfg(feature = "getblocktemplate-rpcs")]
use crate::methods::{GetBlockTemplateRpc, GetBlockTemplateRpcImpl};

pub mod compatibility;
mod tracing_middleware;

#[cfg(test)]
mod tests;

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
        // TODO: use cfg-if to apply trait constraints behind feature flag and remove the `Option`.
        #[cfg(feature = "getblocktemplate-rpcs")] block_verifier: Option<
            BlockVerifier<
                Buffer<
                    BoxService<
                        zebra_state::Request,
                        zebra_state::Response,
                        Box<dyn std::error::Error + Send + Sync>,
                    >,
                    zebra_state::Request,
                >,
                Buffer<
                    BoxService<transaction::Request, transaction::Response, TransactionError>,
                    transaction::Request,
                >,
            >,
        >,
        latest_chain_tip: Tip,
        network: Network,
    ) -> (JoinHandle<()>, JoinHandle<()>)
    where
        Version: ToString + Clone,
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

            // Create handler compatible with V1 and V2 RPC protocols
            let mut io: MetaIoHandler<(), _> =
                MetaIoHandler::new(Compatibility::Both, TracingMiddleware);

            #[cfg(feature = "getblocktemplate-rpcs")]
            {
                if let Some(block_verifier) = block_verifier {
                    let block_verifier = ServiceBuilder::new().service(block_verifier);

                    // Initialize the getblocktemplate rpc methods
                    let get_block_template_rpc_impl = GetBlockTemplateRpcImpl::new(
                        mempool.clone(),
                        state.clone(),
                        latest_chain_tip.clone(),
                        block_verifier,
                    );

                    io.extend_with(get_block_template_rpc_impl.to_delegate());
                }
            }

            // Initialize the rpc methods with the zebra version
            let (rpc_impl, rpc_tx_queue_task_handle) = RpcImpl::new(
                app_version,
                network,
                config.debug_force_finished_sync,
                mempool,
                state,
                latest_chain_tip,
            );

            io.extend_with(rpc_impl.to_delegate());

            // If zero, automatically scale threads to the number of CPU cores
            let mut parallel_cpu_threads = config.parallel_cpu_threads;
            if parallel_cpu_threads == 0 {
                parallel_cpu_threads = num_cpus::get();
            }

            // The server is a blocking task, which blocks on executor shutdown.
            // So we need to create and spawn it on a std::thread, inside a tokio blocking task.
            // (Otherwise tokio panics when we shut down the RPC server.)
            let span = Span::current();
            let server = move || {
                span.in_scope(|| {
                    // Use a different tokio executor from the rest of Zebra,
                    // so that large RPCs and any task handling bugs don't impact Zebra.
                    //
                    // TODO:
                    // - return server.close_handle(), which can shut down the RPC server,
                    //   and add it to the server tests
                    let server = ServerBuilder::new(io)
                        .threads(parallel_cpu_threads)
                        // TODO: disable this security check if we see errors from lightwalletd
                        //.allowed_hosts(DomainsValidation::Disabled)
                        .request_middleware(FixHttpRequestMiddleware)
                        .start_http(&listen_addr)
                        .expect("Unable to start RPC server");

                    info!("Opened RPC endpoint at {}", server.address());

                    server.wait();

                    info!("Stopping RPC endpoint");
                })
            };

            (
                tokio::task::spawn_blocking(|| {
                    let thread_handle = std::thread::spawn(server);

                    // Propagate panics from the inner std::thread to the outer tokio blocking task
                    match thread_handle.join() {
                        Ok(()) => (),
                        Err(panic_object) => panic::resume_unwind(panic_object),
                    }
                }),
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
