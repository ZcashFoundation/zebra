//! Type implementations for the scan request

use color_eyre::{eyre::eyre, Report};
use tonic::Status;

use zcash_client_backend::encoding::decode_extended_full_viewing_key;
use zcash_primitives::{constants::*, zip32::DiversifiableFullViewingKey};

use zebra_chain::parameters::Network;

use crate::{
    auth::{KeyHash, ViewingKey},
    zebra_scan_service::{RawTransaction, ScanRequest, ScanResponse},
};

// TODO: Add key parsing for `ViewingKey` to zebra-chain
//       enum ViewingKey {
//          SaplingIvk(SaplingIvk),
//          ..
//       }
/// Decodes a Sapling extended full viewing key and returns a Sapling diversifiable full viewing key
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
