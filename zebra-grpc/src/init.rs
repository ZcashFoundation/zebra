//! Initializing the scanner gRPC service.

use std::pin::Pin;

use color_eyre::{eyre::eyre, Report};
use futures::stream::FuturesOrdered;
use tokio::task::JoinHandle;
use tokio_stream::Stream;
use tonic::{transport::Server, Request, Response, Status};
use tracing::Instrument;

use zcash_primitives::{constants::*, zip32::DiversifiableFullViewingKey};

use zcash_client_backend::encoding::decode_extended_full_viewing_key;

use zebra_chain::parameters::Network;

use crate::{
    auth::{KeyHash, ViewingKey, ViewingKeyWithHash},
    zebra_scan_service::{
        zebra_scan_rpc_server::{ZebraScanRpc, ZebraScanRpcServer},
        RawTransaction, ScanRequest, ScanResponse,
    },
};

// TODO: Add key parsing for `ViewingKey` to zebra-chain
//       enum ViewingKey {
//          SaplingIvk(SaplingIvk),
//          ..
//       }
pub fn sapling_key_to_scan_block_key(
    sapling_key: &str,
    network: Network,
) -> Result<DiversifiableFullViewingKey, Report> {
    let hrp = if network.is_a_test_network() {
        // Assume custom testnets have the same HRP
        //
        // TODO: add the regtest HRP here
        testnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY
    } else {
        mainnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY
    };
    Ok(decode_extended_full_viewing_key(hrp, sapling_key)
        .map_err(|e| eyre!(e))?
        .to_diversifiable_full_viewing_key())
}

impl ScanRequest {
    /// Returns true if there are no keys or key hashes in this request
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.key_hashes.is_empty()
    }

    /// Parses the keys in this request into known viewing key types
    pub fn keys(&self, network: Network) -> Result<Vec<ViewingKey>, Status> {
        let mut viewing_keys = vec![];

        for key in &self.keys {
            // TODO: Add a FromStr impl for ViewingKey
            let viewing_key = sapling_key_to_scan_block_key(key, network)
                .map_err(|_| Status::invalid_argument("could not parse viewing key"))?
                .into();

            viewing_keys.push(viewing_key);
        }

        Ok(viewing_keys)
    }

    /// Parses the keys in this request into known viewing key types
    pub fn key_hashes(&self) -> Result<Vec<KeyHash>, Status> {
        let mut key_hashes = vec![];

        for key_hash in &self.key_hashes {
            let key_hash = key_hash
                .parse()
                .map_err(|_| Status::invalid_argument("could not parse key hash"))?;

            key_hashes.push(key_hash);
        }

        Ok(key_hashes)
    }
}

impl ScanResponse {
    /// Creates a scan response with relevant transaction results
    pub fn results(results: Vec<RawTransaction>) -> Self {
        Self { results }
    }
}

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
