//! A JSON-RPC 1.0 & 2.0 endpoint for Zebra.
//!
//! This endpoint is compatible with clients that incorrectly send
//! `"jsonrpc" = 1.0` fields in JSON-RPC 1.0 requests,
//! such as `lightwalletd`.
//!
//! See the full list of
//! [Differences between JSON-RPC 1.0 and 2.0.](https://www.simple-is-better.org/rpc/#differences-between-1-0-and-2-0)

use std::{fmt, panic, thread::available_parallelism};

use cookie::Cookie;
use http_request_compatibility::With;
use jsonrpc_core::{Compatibility, MetaIoHandler};
use jsonrpc_http_server::{CloseHandle, ServerBuilder};
use tokio::task::JoinHandle;
use tower::Service;
use tracing::*;

use tonic::{
    transport::{Channel, Server, Uri},
    Request,
};
use warp::Filter;

use zebra_chain::{
    block, chain_sync_status::ChainSyncStatus, chain_tip::ChainTip, parameters::Network,
};
use zebra_network::AddressBookPeers;
use zebra_node_services::mempool;

use crate::{
    config::Config,
    methods::{GrpcImpl, Rpc, RpcImpl},
    server::{
        endpoint_client::EndpointClient, endpoint_server::EndpointServer,
        http_request_compatibility::HttpRequestMiddleware,
        rpc_call_compatibility::FixRpcResponseMiddleware,
    },
};

#[cfg(feature = "getblocktemplate-rpcs")]
use crate::methods::{GetBlockTemplateRpc, GetBlockTemplateRpcImpl};

pub mod cookie;
pub mod http_request_compatibility;
pub mod jsonrpc;
pub mod rpc_call_compatibility;

#[cfg(test)]
mod tests;

// The generated endpoint proto
tonic::include_proto!("zebra.endpoint.rpc");

/// The file descriptor set for the generated endpoint proto
pub(crate) const FILE_DESCRIPTOR_SET: &[u8] =
    tonic::include_file_descriptor_set!("endpoint_descriptor");

/// Zebra RPC Server
#[derive(Clone)]
pub struct RpcServer {
    /// The RPC config.
    config: Config,

    /// The configured network.
    network: Network,

    /// Zebra's application version, with build metadata.
    build_version: String,

    /// A handle that shuts down the RPC server.
    close_handle: CloseHandle,
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
                &"CloseHandle",
            )
            .finish()
    }
}

/// The message to log when logging the RPC server's listen address
pub const OPENED_RPC_ENDPOINT_MSG: &str = "Opened RPC endpoint at ";

impl RpcServer {
    /// Start a new RPC server endpoint using the supplied configs and services.
    ///
    /// `build_version` and `user_agent` are version strings for the application,
    /// which are used in RPC responses.
    ///
    /// Returns [`JoinHandle`]s for the RPC server and `sendrawtransaction` queue tasks,
    /// and a [`RpcServer`] handle, which can be used to shut down the RPC server task.
    //
    // TODO:
    // - put some of the configs or services in their own struct?
    // - replace VersionString with semver::Version, and update the tests to provide valid versions
    #[allow(clippy::too_many_arguments)]
    pub fn spawn<
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
    ) -> (JoinHandle<()>, JoinHandle<()>, Option<Self>)
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
        if let Some(listen_addr) = config.listen_addr {
            let reflection_service = tonic_reflection::server::Builder::configure()
                .register_encoded_file_descriptor_set(FILE_DESCRIPTOR_SET)
                .build_v1()
                .expect("Unable to build reflection service");

            info!("Trying to open RPC endpoint at {}...", listen_addr,);

            // `grpc_server_listen_addr` should be a config argument
            let grpc_server_listen_addr = "127.0.0.1:8080".parse().unwrap();
            let grpc_server_url: Uri = format!("http://{}", grpc_server_listen_addr)
                .parse()
                .unwrap();

            let (grpc, rpc_tx_queue_task_handle) = GrpcImpl::new(
                build_version.clone(),
                user_agent.clone(),
                network.clone(),
                config.debug_force_finished_sync,
                #[cfg(feature = "getblocktemplate-rpcs")]
                mining_config.debug_like_zcashd,
                #[cfg(not(feature = "getblocktemplate-rpcs"))]
                true,
                mempool,
                state,
                latest_chain_tip,
            );

            // Start the gRPC server
            let _ = tokio::spawn(async move {
                let server_instance = Server::builder()
                    .accept_http1(true)
                    .add_service(reflection_service)
                    .add_service(EndpointServer::new(grpc))
                    .serve(grpc_server_listen_addr)
                    .await
                    .expect("Unable to start RPC server");

                info!("Started gRPC server at {}", grpc_server_listen_addr);
                server_instance
            });

            // Create the middleware for the HTTP request
            let middleware = if config.enable_cookie_auth {
                let cookie = Cookie::default();
                cookie::write_to_disk(&cookie, &config.cookie_dir)
                    .expect("Zebra must be able to write the auth cookie to the disk");
                HttpRequestMiddleware::default().with(cookie)
            } else {
                HttpRequestMiddleware::default()
            };

            // Create the warp proxy server
            let rpc_server_task_handle = tokio::spawn(async move {
                let grpc_channel = Channel::builder(grpc_server_url)
                    .connect()
                    .await
                    .expect("Unable to connect to gRPC server");

                let grpc_client = EndpointClient::new(grpc_channel);
                let middleware = std::sync::Arc::new(middleware);

                let proxy = warp::post()
                    .and(warp::path::end())
                    .and(warp::body::json())
                    .and(warp::header::headers_cloned())
                    .and(with_grpc_client(grpc_client)) // Pass the Arc directly
                    .and_then(move |request, headers, grpc_client| {
                        let middleware = std::sync::Arc::clone(&middleware);
                        async move {
                            middleware
                                .handle_request(request, headers, grpc_client)
                                .await
                        }
                    });

                warp::serve(proxy).run(listen_addr).await;

                info!("{OPENED_RPC_ENDPOINT_MSG}{}", listen_addr);
            });

            (
                rpc_server_task_handle,
                rpc_tx_queue_task_handle,
                //Some(rpc_server_handle),
                None,
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
    fn shutdown_blocking_inner(close_handle: CloseHandle, config: Config) {
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

// Warp Filter to pass the gRPC client
fn with_grpc_client(
    grpc_client: EndpointClient<Channel>,
) -> impl Filter<Extract = (EndpointClient<Channel>,), Error = std::convert::Infallible> + Clone {
    warp::any().map(move || grpc_client.clone())
}
