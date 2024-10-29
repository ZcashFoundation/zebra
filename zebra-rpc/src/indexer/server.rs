//! A tonic RPC server for Zebra's indexer API.

use std::net::SocketAddr;

use tokio::task::JoinHandle;
use tonic::transport::{server::TcpIncoming, Server};
use tower::BoxError;
use zebra_chain::chain_tip::ChainTip;

use super::indexer_server::IndexerServer;

type ServerTask = JoinHandle<Result<(), BoxError>>;

/// Indexer RPC service.
pub struct IndexerRPC<ReadStateService, Tip>
where
    ReadStateService: tower::Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <ReadStateService as tower::Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    _read_state: ReadStateService,
    pub(super) chain_tip_change: Tip,
}

/// Initializes the indexer RPC server
pub async fn init<ReadStateService, Tip>(
    listen_addr: SocketAddr,
    _read_state: ReadStateService,
    chain_tip_change: Tip,
) -> Result<(ServerTask, SocketAddr), BoxError>
where
    ReadStateService: tower::Service<
            zebra_state::ReadRequest,
            Response = zebra_state::ReadResponse,
            Error = BoxError,
        > + Clone
        + Send
        + Sync
        + 'static,
    <ReadStateService as tower::Service<zebra_state::ReadRequest>>::Future: Send,
    Tip: ChainTip + Clone + Send + Sync + 'static,
{
    let indexer_service = IndexerRPC {
        _read_state,
        chain_tip_change,
    };

    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(crate::indexer::FILE_DESCRIPTOR_SET)
        .build_v1()
        .unwrap();

    tracing::info!("Trying to open indexer RPC endpoint at {}...", listen_addr,);

    let tcp_listener = tokio::net::TcpListener::bind(listen_addr).await?;
    let listen_addr = tcp_listener.local_addr()?;
    let incoming = TcpIncoming::from_listener(tcp_listener, true, None)?;

    let server_task: JoinHandle<Result<(), BoxError>> = tokio::spawn(async move {
        Server::builder()
            .add_service(reflection_service)
            .add_service(IndexerServer::new(indexer_service))
            .serve_with_incoming(incoming)
            .await?;

        Ok(())
    });

    Ok((server_task, listen_addr))
}
