//! A tonic RPC server for Zebra's indexer API.

use std::net::SocketAddr;

use tokio::task::JoinHandle;
use tonic::transport::{server::TcpIncoming, Server};
use tower::BoxError;

use super::indexer_server::IndexerServer;

type ServerTask = JoinHandle<Result<(), BoxError>>;

/// Indexer RPC service.
pub struct IndexerRPC<ReadStateService>
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
{
    _read_state: ReadStateService,
}

/// Initializes the indexer RPC server
pub async fn init<ReadStateService>(
    listen_addr: SocketAddr,
    _read_state: ReadStateService,
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
{
    let indexer_service = IndexerRPC { _read_state };
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(crate::indexer::FILE_DESCRIPTOR_SET)
        .build()
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
