//! Initializing the scanner gRPC service.

use std::pin::Pin;

use color_eyre::Report;
use futures::stream::FuturesOrdered;
use tokio::task::JoinHandle;
use tokio_stream::Stream;
use tonic::{transport::Server, Request, Response, Status};
use tracing::Instrument;

use zebra_chain::parameters::Network;

use crate::{
    auth::{ViewingKey, ViewingKeyWithHash},
    zebra_scan_service::{
        zebra_scan_rpc_server::{ZebraScanRpc, ZebraScanRpcServer},
        ScanRequest, ScanResponse,
    },
};

#[derive(Debug, Default)]
/// Implements zebra-scan RPC methods
pub struct ZebraScanRpcImpl {
    network: Network,
}

#[tonic::async_trait]
impl ZebraScanRpc for ZebraScanRpcImpl {
    type ScanStream = Pin<Box<dyn Stream<Item = Result<ScanResponse, Status>> + Send>>;

    async fn scan(
        &self,
        request: Request<ScanRequest>,
    ) -> Result<Response<Self::ScanStream>, Status> {
        let request = request.into_inner();

        if request.is_empty() {
            let msg = "must provide either new keys or hashes of registered keys";
            return Err(Status::invalid_argument(msg));
        }

        // Parse new keys in the request into known viewing key types

        let keys: Vec<ViewingKey> = request.keys(self.network)?;
        let key_hashes = request.key_hashes()?;

        // TODO: Look up key hashes in scanner state, return an error if any are missing

        let _new_keys_with_hashes: Vec<ViewingKeyWithHash> = keys
            .into_iter()
            .map(ViewingKeyWithHash::from)
            // Filter out known/previously-registered keys
            .filter(|key| !key_hashes.contains(&key.hash))
            .collect();

        // TODO: Register authorized keys with the scanner service and stream results to client

        let mut response_stream = FuturesOrdered::new();

        response_stream.push_back(async move { Ok(ScanResponse::results(vec![])) });

        // TODO: Add bidirectional stream so the client can acknowledge results

        Ok(Response::new(Box::pin(response_stream)))
    }
}

/// Initialize the scanner gRPC service, and spawn a task for it.
async fn _spawn_init() -> JoinHandle<Result<(), Report>> {
    tokio::spawn(_init().in_current_span())
}

/// Initialize the scanner gRPC service.
async fn _init() -> Result<(), Report> {
    Server::builder()
        .add_service(ZebraScanRpcServer::new(ZebraScanRpcImpl::default()))
        .serve("127.0.0.1:3031".parse()?)
        .await?;

    Ok(())
}
