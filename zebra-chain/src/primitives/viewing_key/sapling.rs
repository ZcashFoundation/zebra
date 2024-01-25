//! Implements methods on [`ViewingKey`] for parsing Sapling viewing keys

use zcash_client_backend::encoding::{decode_extended_full_viewing_key, Bech32DecodeError};
use zcash_primitives::{constants::*, zip32::DiversifiableFullViewingKey};

use super::*;
use crate::parameters::Network;

impl Network {
    /// Returns the human-readable prefix for an Zcash Sapling extended full viewing key
    /// for this network.
    fn sapling_efvk_hrp(&self) -> &'static str {
        if self.is_a_test_network() {
            // Assume custom testnets have the same HRP
            //
            // TODO: add the regtest HRP here
            testnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY
        } else {
            mainnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY
        }
    }
}

impl ViewingKey {
    /// Decodes a Sapling extended full viewing key and returns a Sapling diversifiable full viewing key
    pub fn parse_extended_full_viewing_key(
        sapling_key: &str,
        network: Network,
    ) -> Result<Box<DiversifiableFullViewingKey>, Bech32DecodeError> {
        let hrp = network.sapling_efvk_hrp();
        Ok(Box::new(
            decode_extended_full_viewing_key(hrp, sapling_key)?.to_diversifiable_full_viewing_key(),
        ))
    }
}
