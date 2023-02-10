//! A JSON-RPC 1.0 & 2.0 endpoint for Zebra.
//!
//! This endpoint is compatible with clients that incorrectly send
//! `"jsonrpc" = 1.0` fields in JSON-RPC 1.0 requests,
//! such as `lightwalletd`.
//!
//! See the full list of
//! [Differences between JSON-RPC 1.0 and 2.0.](https://www.simple-is-better.org/rpc/#differences-between-1-0-and-2-0)

use std::{fmt, panic};

use jsonrpc_core::{Compatibility, MetaIoHandler};
use jsonrpc_http_server::{CloseHandle, ServerBuilder};
use tokio::task::JoinHandle;
use tower::{buffer::Buffer, Service};

use tracing::{Instrument, *};

use zebra_chain::{
    block, chain_sync_status::ChainSyncStatus, chain_tip::ChainTip, parameters::Network,
};
use zebra_network::AddressBookPeers;
use zebra_node_services::mempool;

use crate::{
    config::Config,
    methods::{Rpc, RpcImpl},
    server::{
        http_request_compatibility::FixHttpRequestMiddleware,
        rpc_call_compatibility::FixRpcResponseMiddleware,
    },
};

#[cfg(feature = "getblocktemplate-rpcs")]
use crate::methods::{get_block_template_rpcs, GetBlockTemplateRpc, GetBlockTemplateRpcImpl};

pub mod http_request_compatibility;
pub mod rpc_call_compatibility;

#[cfg(test)]
mod tests;

/// Zebra RPC Server
#[derive(Clone)]
pub struct RpcServer {
    config: Config,
    network: Network,
    app_version: String,
    close_handle: CloseHandle,
}

impl fmt::Debug for RpcServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcServer")
            .field("config", &self.config)
            .field("network", &self.network)
            .field("app_version", &self.app_version)
            .field(
                "close_handle",
                // TODO: when it stabilises, use std::any::type_name_of_val(&self.close_handle)
                &"CloseHandle",
            )
            .finish()
    }
}

impl RpcServer {
    /// Start a new RPC server endpoint using the supplied configs and services.
    /// `app_version` is a version string for the application, which is used in RPC responses.
    ///
    /// Returns [`JoinHandle`]s for the RPC server and `sendrawtransaction` queue tasks,
    /// and a [`RpcServer`] handle, which can be used to shut down the RPC server task.
    //
    // TODO: put some of the configs or services in their own struct?
    #[allow(clippy::too_many_arguments)]
    pub fn spawn<Version, Mempool, State, Tip, ChainVerifier, SyncStatus, AddressBook>(
        config: Config,
        #[cfg(feature = "getblocktemplate-rpcs")]
        mining_config: get_block_template_rpcs::config::Config,
        #[cfg(not(feature = "getblocktemplate-rpcs"))]
        #[allow(unused_variables)]
        mining_config: (),
        app_version: Version,
        mempool: Buffer<Mempool, mempool::Request>,
        state: State,
        #[cfg_attr(not(feature = "getblocktemplate-rpcs"), allow(unused_variables))]
        chain_verifier: ChainVerifier,
        #[cfg_attr(not(feature = "getblocktemplate-rpcs"), allow(unused_variables))]
        sync_status: SyncStatus,
        #[cfg_attr(not(feature = "getblocktemplate-rpcs"), allow(unused_variables))]
        address_book: AddressBook,
        latest_chain_tip: Tip,
        network: Network,
    ) -> (JoinHandle<()>, JoinHandle<()>, Option<Self>)
    where
        Version: ToString + Clone + Send + 'static,
        Mempool: tower::Service<
                mempool::Request,
                Response = mempool::Response,
                Error = zebra_node_services::BoxError,
            > + 'static,
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
        ChainVerifier: Service<
                zebra_consensus::Request,
                Response = block::Hash,
                Error = zebra_consensus::BoxError,
            > + Clone
            + Send
            + Sync
            + 'static,
        <ChainVerifier as Service<zebra_consensus::Request>>::Future: Send,
        SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
        AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
    {
        if let Some(listen_addr) = config.listen_addr {
            info!("Trying to open RPC endpoint at {}...", listen_addr,);

            // Create handler compatible with V1 and V2 RPC protocols
            let mut io: MetaIoHandler<(), _> =
                MetaIoHandler::new(Compatibility::Both, FixRpcResponseMiddleware);

            #[cfg(feature = "getblocktemplate-rpcs")]
            {
                // Prevent loss of miner funds due to an unsupported or incorrect address type.
                if let Some(miner_address) = mining_config.miner_address {
                    assert_eq!(
                        miner_address.network(),
                        network,
                        "incorrect miner address config: {miner_address} \
                         network.network {network} and miner address network {} must match",
                        miner_address.network(),
                    );
                }

                // Initialize the getblocktemplate rpc method handler
                let get_block_template_rpc_impl = GetBlockTemplateRpcImpl::new(
                    network,
                    mining_config.clone(),
                    mempool.clone(),
                    state.clone(),
                    latest_chain_tip.clone(),
                    chain_verifier,
                    sync_status,
                    address_book,
                );

                io.extend_with(get_block_template_rpc_impl.to_delegate());
            }

            // Initialize the rpc methods with the zebra version
            let (rpc_impl, rpc_tx_queue_task_handle) = RpcImpl::new(
                app_version.clone(),
                network,
                config.debug_force_finished_sync,
                #[cfg(feature = "getblocktemplate-rpcs")]
                mining_config.debug_like_zcashd,
                #[cfg(not(feature = "getblocktemplate-rpcs"))]
                true,
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
            // So we need to start it in a std::thread.
            // (Otherwise tokio panics on RPC port conflict, which shuts down the RPC server.)
            let span = Span::current();
            let start_server = move || {
                span.in_scope(|| {
                    // Use a different tokio executor from the rest of Zebra,
                    // so that large RPCs and any task handling bugs don't impact Zebra.
                    let server_instance = ServerBuilder::new(io)
                        .threads(parallel_cpu_threads)
                        // TODO: disable this security check if we see errors from lightwalletd
                        //.allowed_hosts(DomainsValidation::Disabled)
                        .request_middleware(FixHttpRequestMiddleware)
                        .start_http(&listen_addr)
                        .expect("Unable to start RPC server");

                    info!("Opened RPC endpoint at {}", server_instance.address());

                    let close_handle = server_instance.close_handle();

                    let rpc_server_handle = RpcServer {
                        config,
                        network,
                        app_version: app_version.to_string(),
                        close_handle,
                    };

                    (server_instance, rpc_server_handle)
                })
            };

            // Propagate panics from the std::thread
            let (server_instance, rpc_server_handle) = match std::thread::spawn(start_server).join()
            {
                Ok(rpc_server) => rpc_server,
                Err(panic_object) => panic::resume_unwind(panic_object),
            };

            // The server is a blocking task, which blocks on executor shutdown.
            // So we need to wait on it on a std::thread, inside a tokio blocking task.
            // (Otherwise tokio panics when we shut down the RPC server.)
            let span = Span::current();
            let wait_on_server = move || {
                span.in_scope(|| {
                    server_instance.wait();

                    info!("Stopped RPC endpoint");
                })
            };

            let span = Span::current();
            let rpc_server_task_handle = tokio::task::spawn_blocking(move || {
                let thread_handle = std::thread::spawn(wait_on_server);

                // Propagate panics from the inner std::thread to the outer tokio blocking task
                span.in_scope(|| match thread_handle.join() {
                    Ok(()) => (),
                    Err(panic_object) => panic::resume_unwind(panic_object),
                })
            });

            (
                rpc_server_task_handle,
                rpc_tx_queue_task_handle,
                Some(rpc_server_handle),
            )
        } else {
            // There is no RPC port, so the RPC tasks do nothing.
            (
                tokio::task::spawn(futures::future::pending().in_current_span()),
                tokio::task::spawn(futures::future::pending().in_current_span()),
                None,
            )
        }
    }

    /// Shut down this RPC server, blocking the current thread.
    ///
    /// This method can be called from within a tokio executor without panicking.
    /// But it is blocking, so `shutdown()` should be used instead.
    pub fn shutdown_blocking(&self) {
        Self::shutdown_blocking_inner(self.close_handle.clone())
    }

    /// Shut down this RPC server asynchronously.
    /// Returns a task that completes when the server is shut down.
    pub fn shutdown(&self) -> JoinHandle<()> {
        let close_handle = self.close_handle.clone();

        let span = Span::current();
        tokio::task::spawn_blocking(move || {
            span.in_scope(|| Self::shutdown_blocking_inner(close_handle))
        })
    }

    /// Shuts down this RPC server using its `close_handle`.
    ///
    /// See `shutdown_blocking()` for details.
    fn shutdown_blocking_inner(close_handle: CloseHandle) {
        // The server is a blocking task, so it can't run inside a tokio thread.
        // See the note at wait_on_server.
        let span = Span::current();
        let wait_on_shutdown = move || {
            span.in_scope(|| {
                info!("Stopping RPC server");
                close_handle.clone().close();
                debug!("Stopped RPC server");
            })
        };

        let span = Span::current();
        let thread_handle = std::thread::spawn(wait_on_shutdown);

        // Propagate panics from the inner std::thread to the outer tokio blocking task
        span.in_scope(|| match thread_handle.join() {
            Ok(()) => (),
            Err(panic_object) => panic::resume_unwind(panic_object),
        })
    }
}

impl Drop for RpcServer {
    fn drop(&mut self) {
        // Block on shutting down, propagating panics.
        // This can take around 150 seconds.
        //
        // Without this shutdown, Zebra's RPC unit tests sometimes crashed with memory errors.
        self.shutdown_blocking();
    }
}
