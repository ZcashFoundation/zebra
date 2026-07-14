//! A tonic gRPC server for the lightwalletd `CompactTxStreamer` client API.

use std::net::SocketAddr;

use tokio::task::JoinHandle;
use tonic::transport::{server::TcpIncoming, Server};
use tower::BoxError;
use zebra_chain::{chain_tip::ChainTip, parameters::Network};
use zebra_node_services::mempool::{MempoolService, MempoolTxSubscriber};
use zebra_state::ReadState;

use crate::{
    lightwalletd::compact_tx_streamer_server::CompactTxStreamerServer,
    methods::RpcServer as RpcMethods, server::OPENED_RPC_ENDPOINT_MSG,
};

type ServerTask = JoinHandle<Result<(), BoxError>>;

/// Lightwalletd gRPC service.
///
/// Most methods are implemented by calling Zebra's JSON-RPC methods in-process, in the
/// same way lightwalletd calls them on zcashd. Compact blocks and mempool streams are
/// built directly from the state and mempool services.
pub struct LightwalletdRPC<Rpc, ReadStateService, Mempool, Tip>
where
    Rpc: RpcMethods + Clone,
    ReadStateService: ReadState,
    Mempool: MempoolService,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    pub(super) rpc: Rpc,
    pub(super) read_state: ReadStateService,
    pub(super) mempool: Mempool,
    pub(super) latest_chain_tip: Tip,
    pub(super) mempool_change: MempoolTxSubscriber,
    pub(super) network: Network,
}

/// Initializes the lightwalletd gRPC server.
#[tracing::instrument(skip_all)]
pub async fn init<Rpc, ReadStateService, Mempool, Tip>(
    listen_addr: SocketAddr,
    rpc: Rpc,
    read_state: ReadStateService,
    mempool: Mempool,
    latest_chain_tip: Tip,
    mempool_change: MempoolTxSubscriber,
    network: Network,
) -> Result<(ServerTask, SocketAddr), BoxError>
where
    Rpc: RpcMethods + Clone,
    ReadStateService: ReadState,
    Mempool: MempoolService,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    let lightwalletd_service = LightwalletdRPC {
        rpc,
        read_state,
        mempool,
        latest_chain_tip,
        mempool_change,
        network,
    };

    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(crate::lightwalletd::FILE_DESCRIPTOR_SET)
        .build_v1()?;

    tracing::info!(
        "Trying to open lightwalletd gRPC endpoint at {}...",
        listen_addr,
    );

    let tcp_listener = tokio::net::TcpListener::bind(listen_addr).await?;

    let listen_addr = tcp_listener.local_addr()?;
    tracing::info!("{OPENED_RPC_ENDPOINT_MSG}{}", listen_addr);

    let server_task: JoinHandle<Result<(), BoxError>> = tokio::spawn(async move {
        Server::builder()
            .add_service(reflection_service)
            .add_service(CompactTxStreamerServer::new(lightwalletd_service))
            .serve_with_incoming(TcpIncoming::from(tcp_listener))
            .await?;

        Ok(())
    });

    Ok((server_task, listen_addr))
}
