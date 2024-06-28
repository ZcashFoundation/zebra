//! A tonic RPC server for Zebra's indexer API.

use std::{net::SocketAddr, pin::Pin};

use futures::Stream;
use tokio::task::JoinHandle;
use tonic::{
    transport::{server::TcpIncoming, Server},
    Response, Status,
};
use tower::BoxError;
use zebra_state::ReadStateService;

use super::{
    indexer_server::{Indexer, IndexerServer},
    ChainTip, Empty,
};

type ServerTask = JoinHandle<Result<(), tonic::transport::Error>>;

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
    read_state: ReadStateService,
}

#[tonic::async_trait]
impl<ReadStateService> Indexer for IndexerRPC<ReadStateService>
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
    type ChainTipChangeStream = Pin<Box<dyn Stream<Item = Result<ChainTip, Status>> + Send>>;

    async fn chain_tip_change(
        &self,
        request: tonic::Request<Empty>,
    ) -> Result<Response<Self::ChainTipChangeStream>, Status> {
        todo!()
    }
}
/// Initializes the indexer RPC server
pub async fn init<ScanService>(
    listen_addr: SocketAddr,
    read_state: ReadStateService,
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
    let indexer_service = IndexerRPC { read_state };
    let reflection_service = tonic_reflection::server::Builder::configure()
        .register_encoded_file_descriptor_set(crate::indexer::FILE_DESCRIPTOR_SET)
        .build()
        .unwrap();

    let tcp_listener = tokio::net::TcpListener::bind(listen_addr).await?;
    let listen_addr = tcp_listener.local_addr()?;
    let incoming = TcpIncoming::from_listener(tcp_listener, true, None)?;

    let server_task: JoinHandle<Result<(), tonic::transport::Error>> = tokio::spawn(async move {
        Server::builder()
            .add_service(reflection_service)
            .add_service(IndexerServer::new(indexer_service))
            .serve_with_incoming(incoming)
            .await?;

        Ok(())
    });

    Ok((server_task, listen_addr))
}
