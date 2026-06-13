//! A JSON-RPC 1.0 & 2.0 endpoint for Zebra.
//!
//! This endpoint is compatible with clients that incorrectly send
//! `"jsonrpc" = 1.0` fields in JSON-RPC 1.0 requests,
//! such as `lightwalletd`.
//!
//! See the full list of
//! [Differences between JSON-RPC 1.0 and 2.0.](https://www.simple-is-better.org/rpc/#differences-between-1-0-and-2-0)

use std::{fmt, fs::File, io::BufReader, panic, sync::Arc};

use cookie::Cookie;
use jsonrpsee::server::{
    middleware::rpc::RpcServiceBuilder, serve_with_graceful_shutdown, stop_channel, Server,
    ServerHandle,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_rustls::{rustls::ServerConfig as RustlsServerConfig, TlsAcceptor};
use tracing::*;

use zebra_chain::{
    block::MAX_BLOCK_BYTES, chain_sync_status::ChainSyncStatus, chain_tip::ChainTip,
    parameters::Network,
};
use zebra_consensus::router::service_trait::BlockVerifierService;
use zebra_network::AddressBookPeers;
use zebra_node_services::mempool::MempoolService;
use zebra_state::{ReadState as ReadStateService, State as StateService};

use crate::{
    config,
    methods::{RpcImpl, RpcServer as _},
    server::{
        http_request_compatibility::HttpRequestMiddlewareLayer,
        rpc_call_compatibility::FixRpcResponseMiddleware, rpc_metrics::RpcMetricsMiddleware,
        rpc_tracing::RpcTracingMiddleware,
    },
};

pub mod cookie;
pub mod error;
pub mod http_request_compatibility;
pub mod rpc_call_compatibility;
pub mod rpc_metrics;
pub mod rpc_tracing;

#[cfg(test)]
mod tests;

/// Zebra RPC Server
#[derive(Clone)]
pub struct RpcServer {
    /// The RPC config.
    config: config::rpc::Config,

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
    /// Starts the RPC server.
    ///
    /// `build_version` and `user_agent` are version strings for the application,
    /// which are used in RPC responses.
    ///
    /// Returns [`JoinHandle`]s for the RPC server and `sendrawtransaction` queue tasks,
    /// and a [`RpcServer`] handle, which can be used to shut down the RPC server task.
    ///
    /// # Panics
    ///
    /// - If [`Config::listen_addr`](config::rpc::Config::listen_addr) is `None`.
    //
    // TODO:
    // - replace VersionString with semver::Version, and update the tests to provide valid versions
    #[allow(clippy::too_many_arguments)]
    pub async fn start<
        Mempool,
        State,
        ReadState,
        Tip,
        BlockVerifierRouter,
        SyncStatus,
        AddressBook,
    >(
        rpc: RpcImpl<Mempool, State, ReadState, Tip, AddressBook, BlockVerifierRouter, SyncStatus>,
        conf: config::rpc::Config,
    ) -> Result<ServerTask, tower::BoxError>
    where
        Mempool: MempoolService,
        State: StateService,
        ReadState: ReadStateService,
        Tip: ChainTip + Clone + Send + Sync + 'static,
        AddressBook: AddressBookPeers + Clone + Send + Sync + 'static,
        BlockVerifierRouter: BlockVerifierService,
        SyncStatus: ChainSyncStatus + Clone + Send + Sync + 'static,
    {
        let listen_addr = conf
            .listen_addr
            .expect("caller should make sure listen_addr is set");

        // The largest RPC request is submitblock, which sends a full block
        // as a hex string (2x MAX_BLOCK_BYTES) plus a small JSON-RPC wrapper.
        let max_request_body_size = (MAX_BLOCK_BYTES as usize) * 2 + 1024;

        let http_middleware_layer = if conf.enable_cookie_auth {
            let cookie = Cookie::default();
            cookie::write_to_disk(&cookie, &conf.cookie_dir, Some(&conf.cookie_file_name))
                .expect("Zebra must be able to write the auth cookie to the disk");
            HttpRequestMiddlewareLayer::new(Some(cookie), max_request_body_size)
        } else {
            HttpRequestMiddlewareLayer::new(None, max_request_body_size)
        };

        let http_middleware = tower::ServiceBuilder::new().layer(http_middleware_layer);

        let rpc_middleware = RpcServiceBuilder::new()
            .rpc_logger(1024)
            .layer_fn(FixRpcResponseMiddleware::new)
            .layer_fn(RpcMetricsMiddleware::new)
            .layer_fn(RpcTracingMiddleware::new);

        if let Some(tls) = conf.tls.clone() {
            let tls_config = load_tls_config(&tls)?;
            let listener = TcpListener::bind(listen_addr).await?;
            let local_addr = listener.local_addr()?;
            let acceptor = TlsAcceptor::from(tls_config);
            let service_builder = Server::builder()
                .http_only()
                .set_http_middleware(http_middleware)
                .set_rpc_middleware(rpc_middleware)
                .max_response_body_size(
                    conf.max_response_body_size
                        .try_into()
                        .expect("should be valid"),
                )
                .to_service_builder();
            let methods = rpc.into_rpc();
            let (stop_handle, server_handle) = stop_channel();

            info!("{OPENED_RPC_ENDPOINT_MSG}{local_addr}");

            return Ok(tokio::spawn(async move {
                loop {
                    let (socket, remote_addr) = tokio::select! {
                        result = listener.accept() => match result {
                            Ok(connection) => connection,
                            Err(error) => return Err(error.into()),
                        },
                        _ = stop_handle.clone().shutdown() => break,
                    };

                    let acceptor = acceptor.clone();
                    let service = service_builder
                        .clone()
                        .build(methods.clone(), stop_handle.clone());
                    let stopped = stop_handle.clone().shutdown();

                    tokio::spawn(async move {
                        match acceptor.accept(socket).await {
                            Ok(stream) => {
                                if let Err(error) =
                                    serve_with_graceful_shutdown(stream, service, stopped).await
                                {
                                    warn!(
                                        ?error,
                                        %remote_addr,
                                        "TLS RPC connection terminated with an error"
                                    );
                                }
                            }
                            Err(error) => {
                                warn!(
                                    ?error,
                                    %remote_addr,
                                    "TLS RPC handshake failed"
                                );
                            }
                        }
                    });
                }

                drop(server_handle);
                Ok(())
            }));
        }

        let server = Server::builder()
            .http_only()
            .set_http_middleware(http_middleware)
            .set_rpc_middleware(rpc_middleware)
            .max_response_body_size(
                conf.max_response_body_size
                    .try_into()
                    .expect("should be valid"),
            )
            .build(listen_addr)
            .await?;

        info!("{OPENED_RPC_ENDPOINT_MSG}{}", server.local_addr()?);

        Ok(tokio::spawn(async move {
            server.start(rpc.into_rpc()).stopped().await;
            Ok(())
        }))
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
    fn shutdown_blocking_inner(close_handle: ServerHandle, config: config::rpc::Config) {
        // The server is a blocking task, so it can't run inside a tokio thread.
        // See the note at wait_on_server.
        let span = Span::current();
        let wait_on_shutdown = move || {
            span.in_scope(|| {
                if config.enable_cookie_auth {
                    if let Err(err) =
                        cookie::remove_from_disk(&config.cookie_dir, Some(&config.cookie_file_name))
                    {
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

fn load_tls_config(
    tls: &config::rpc::TlsConfig,
) -> Result<Arc<RustlsServerConfig>, tower::BoxError> {
    let cert_file = File::open(&tls.cert_file).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "could not open RPC TLS certificate file {}: {error}",
                tls.cert_file.display()
            ),
        )
    })?;
    let key_file = File::open(&tls.key_file).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "could not open RPC TLS private key file {}: {error}",
                tls.key_file.display()
            ),
        )
    })?;

    let cert_chain: Vec<CertificateDer<'static>> =
        rustls_pemfile::certs(&mut BufReader::new(cert_file)).collect::<Result<_, _>>()?;
    if cert_chain.is_empty() {
        return Err(format!(
            "RPC TLS certificate file {} did not contain any certificates",
            tls.cert_file.display()
        )
        .into());
    }

    let private_key: PrivateKeyDer<'static> =
        rustls_pemfile::private_key(&mut BufReader::new(key_file))?.ok_or_else(|| {
            format!(
                "RPC TLS private key file {} did not contain a usable private key",
                tls.key_file.display()
            )
        })?;

    let crypto_provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = RustlsServerConfig::builder_with_provider(crypto_provider)
        .with_safe_default_protocol_versions()
        .map_err(|error| format!("could not configure RPC TLS protocol versions: {error}"))?
        .with_no_client_auth()
        .with_single_cert(cert_chain, private_key)
        .map_err(|error| format!("could not build RPC TLS server config: {error}"))?;

    Ok(Arc::new(config))
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
