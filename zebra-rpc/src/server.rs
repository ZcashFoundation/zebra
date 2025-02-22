//! A JSON-RPC 1.0 & 2.0 endpoint for Zebra.
//!
//! This endpoint is compatible with clients that incorrectly send
//! `"jsonrpc" = 1.0` fields in JSON-RPC 1.0 requests,
//! such as `lightwalletd`.
//!
//! See the full list of
//! [Differences between JSON-RPC 1.0 and 2.0.](https://www.simple-is-better.org/rpc/#differences-between-1-0-and-2-0)

use std::{fmt, panic};

use cookie::Cookie;
use jsonrpsee::server::middleware::rpc::RpcServiceBuilder;
use jsonrpsee::server::{Server, ServerHandle};
use tokio::{sync::watch, task::JoinHandle};
use tower::Service;
use tracing::*;

use zebra_chain::{
    block, chain_sync_status::ChainSyncStatus, chain_tip::ChainTip, parameters::Network,
};
use zebra_network::AddressBookPeers;
use zebra_node_services::mempool;

use crate::{
    config::Config,
    methods::{LoggedLastEvent, RpcImpl, RpcServer as _},
    server::{
        http_request_compatibility::HttpRequestMiddlewareLayer,
        rpc_call_compatibility::FixRpcResponseMiddleware,
    },
};

#[cfg(feature = "getblocktemplate-rpcs")]
use crate::methods::{GetBlockTemplateRpcImpl, GetBlockTemplateRpcServer};

pub mod cookie;
pub mod error;
pub mod http_request_compatibility;
pub mod rpc_call_compatibility;

#[cfg(test)]
mod tests;

/// Zebra RPC Server
#[derive(Clone)]
pub struct RpcServer {
    /// The RPC config.
    config: Config,

    /// The configured network.
    network: Network,

    /// Zebra's application version, with build metadata.
    build_version: String,

    /// A server handle used to shuts down the RPC server.
    close_handle: ServerHandle,
}

impl fmt::Debug for RpcServer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RpcServer")
            .field("config", &self.config)
            .field("network", &self.network)
            .field("build_version", &self.build_version)
            .field(
                "close_handle",
                // TODO: when it stabilises, use std::any::type_name_of_val(&self.close_handle)
                &"ServerHandle",
            )
            .finish()
    }
}

/// The message to log when logging the RPC server's listen address
pub const OPENED_RPC_ENDPOINT_MSG: &str = "Opened RPC endpoint at ";

type ServerTask = JoinHandle<Result<(), tower::BoxError>>;

impl RpcServer {
    /// Start a new RPC server endpoint using the supplied configs and services.
    ///
    /// `build_version` and `user_agent` are version strings for the application,
    /// which are used in RPC responses.
    ///
    /// Returns [`JoinHandle`]s for the RPC server and `sendrawtransaction` queue tasks,
    /// and a [`RpcServer`] handle, which can be used to shut down the RPC server task.
    ///
    /// # Panics
    ///
    /// - If [`Config::listen_addr`] is `None`.
    //
    // TODO:
    // - put some of the configs or services in their own struct?
    // - replace VersionString with semver::Version, and update the tests to provide valid versions
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn<
        VersionString,
        UserAgentString,
        Mempool,
        State,
        Tip,
        BlockVerifierRouter,
        SyncStatus,
        AddressBook,
    >(
        config: Config,
        #[cfg_attr(not(feature = "getblocktemplate-rpcs"), allow(unused_variables))]
        mining_config: crate::config::mining::Config,
        build_version: VersionString,
        user_agent: UserAgentString,
        mempool: Mempool,
        state: State,
        #[cfg_attr(not(feature = "getblocktemplate-rpcs"), allow(unused_variables))]
        block_verifier_router: BlockVerifierRouter,
        #[cfg_attr(not(feature = "getblocktemplate-rpcs"), allow(unused_variables))]
        sync_status: SyncStatus,
        #[cfg_attr(not(feature = "getblocktemplate-rpcs"), allow(unused_variables))]
        address_book: AddressBook,
        latest_chain_tip: Tip,
        network: Network,
        #[cfg_attr(not(feature = "getblocktemplate-rpcs"), allow(unused_variables))]
        mined_block_sender: Option<watch::Sender<(block::Hash, block::Height)>>,
        last_event: LoggedLastEvent,
    ) -> Result<(ServerTask, JoinHandle<()>), tower::BoxError>
    where
        VersionString: ToString + Clone + Send + 'static,
        UserAgentString: ToString + Clone + Send + 'static,
        Mempool: tower::Service<
                mempool::Request,
                Response = mempool::Response,
                Error = zebra_node_services::BoxError,
            > + Clone
            + Send
            + Sync
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
        BlockVerifierRouter: Service<
                zebra_consensus::Request,
                Response = block::Hash,
                Error = zebra_consensus::BoxError,
            > + Clone
            + Send
            + Sync
            + 'static,
        <BlockVerifierRouter as Service<zebra_consensus::Request>>::Future: Send,
        SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
        AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
    {
        let listen_addr = config
            .listen_addr
            .expect("caller should make sure listen_addr is set");

        #[cfg(feature = "getblocktemplate-rpcs")]
        // Initialize the getblocktemplate rpc method handler
        let get_block_template_rpc_impl = GetBlockTemplateRpcImpl::new(
            &network,
            mining_config.clone(),
            mempool.clone(),
            state.clone(),
            latest_chain_tip.clone(),
            block_verifier_router,
            sync_status,
            address_book.clone(),
            mined_block_sender,
        );

        // Initialize the rpc methods with the zebra version
        let (rpc_impl, rpc_tx_queue_task_handle) = RpcImpl::new(
            build_version.clone(),
            user_agent,
            network.clone(),
            config.debug_force_finished_sync,
            #[cfg(feature = "getblocktemplate-rpcs")]
            mining_config.debug_like_zcashd,
            #[cfg(not(feature = "getblocktemplate-rpcs"))]
            true,
            mempool,
            state,
            latest_chain_tip,
            address_book,
            last_event,
        );

        let http_middleware_layer = if config.enable_cookie_auth {
            let cookie = Cookie::default();
            cookie::write_to_disk(&cookie, &config.cookie_dir)
                .expect("Zebra must be able to write the auth cookie to the disk");
            HttpRequestMiddlewareLayer::new(Some(cookie))
        } else {
            HttpRequestMiddlewareLayer::new(None)
        };

        let http_middleware = tower::ServiceBuilder::new().layer(http_middleware_layer);

        let rpc_middleware = RpcServiceBuilder::new()
            .rpc_logger(1024)
            .layer_fn(FixRpcResponseMiddleware::new);

        let server_instance = Server::builder()
            .http_only()
            .set_http_middleware(http_middleware)
            .set_rpc_middleware(rpc_middleware)
            .build(listen_addr)
            .await
            .expect("Unable to start RPC server");
        let addr = server_instance
            .local_addr()
            .expect("Unable to get local address");
        info!("{OPENED_RPC_ENDPOINT_MSG}{}", addr);

        #[cfg(feature = "getblocktemplate-rpcs")]
        let mut rpc_module = rpc_impl.into_rpc();
        #[cfg(not(feature = "getblocktemplate-rpcs"))]
        let rpc_module = rpc_impl.into_rpc();
        #[cfg(feature = "getblocktemplate-rpcs")]
        rpc_module
            .merge(get_block_template_rpc_impl.into_rpc())
            .unwrap();

        let server_task: JoinHandle<Result<(), tower::BoxError>> = tokio::spawn(async move {
            server_instance.start(rpc_module).stopped().await;
            Ok(())
        });
        Ok((server_task, rpc_tx_queue_task_handle))
    }

    /// Shut down this RPC server, blocking the current thread.
    ///
    /// This method can be called from within a tokio executor without panicking.
    /// But it is blocking, so `shutdown()` should be used instead.
    pub fn shutdown_blocking(&self) {
        Self::shutdown_blocking_inner(self.close_handle.clone(), self.config.clone())
    }

    /// Shut down this RPC server asynchronously.
    /// Returns a task that completes when the server is shut down.
    pub fn shutdown(&self) -> JoinHandle<()> {
        let close_handle = self.close_handle.clone();
        let config = self.config.clone();
        let span = Span::current();

        tokio::task::spawn_blocking(move || {
            span.in_scope(|| Self::shutdown_blocking_inner(close_handle, config))
        })
    }

    /// Shuts down this RPC server using its `close_handle`.
    ///
    /// See `shutdown_blocking()` for details.
    fn shutdown_blocking_inner(close_handle: ServerHandle, config: Config) {
        // The server is a blocking task, so it can't run inside a tokio thread.
        // See the note at wait_on_server.
        let span = Span::current();
        let wait_on_shutdown = move || {
            span.in_scope(|| {
                if config.enable_cookie_auth {
                    if let Err(err) = cookie::remove_from_disk(&config.cookie_dir) {
                        warn!(
                            ?err,
                            "unexpectedly could not remove the rpc auth cookie from the disk"
                        )
                    }
                }

                info!("Stopping RPC server");
                let _ = close_handle.stop();
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
