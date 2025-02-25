//! Defines types and implements methods for parsing Sapling viewing keys and converting them to `zebra-chain` types

use sapling_crypto::keys::{FullViewingKey as SaplingFvk, SaplingIvk};
use zcash_client_backend::{
    encoding::decode_extended_full_viewing_key,
    keys::sapling::DiversifiableFullViewingKey as SaplingDfvk,
};
use zcash_protocol::constants::*;

use crate::parameters::Network;

/// A Zcash Sapling viewing key
#[derive(Debug, Clone)]
pub enum SaplingViewingKey {
    /// An incoming viewing key for Sapling
    Ivk(Box<SaplingIvk>),

    /// A full viewing key for Sapling
    Fvk(Box<SaplingFvk>),

    /// A diversifiable full viewing key for Sapling
    Dfvk(Box<SaplingDfvk>),
}

impl SaplingViewingKey {
    /// Accepts an encoded Sapling extended full viewing key to decode
    ///
    /// Returns a [`SaplingViewingKey::Dfvk`] if successful, or None otherwise
    fn parse_extended_full_viewing_key(sapling_key: &str, network: &Network) -> Option<Self> {
        decode_extended_full_viewing_key(network.sapling_efvk_hrp(), sapling_key)
            // this should fail often, so a debug-level log is okay
            .map_err(|err| debug!(?err, "could not decode Sapling extended full viewing key"))
            .ok()
            .map(|efvk| Box::new(efvk.to_diversifiable_full_viewing_key()))
            .map(Self::Dfvk)
    }

    /// Accepts an encoded Sapling diversifiable full viewing key to decode
    ///
    /// Returns a [`SaplingViewingKey::Dfvk`] if successful, or None otherwise
    fn parse_diversifiable_full_viewing_key(
        _sapling_key: &str,
        _network: &Network,
    ) -> Option<Self> {
        // TODO: Parse Sapling diversifiable full viewing key
        None
    }

    /// Accepts an encoded Sapling full viewing key to decode
    ///
    /// Returns a [`SaplingViewingKey::Fvk`] if successful, or None otherwise
    fn parse_full_viewing_key(_sapling_key: &str, _network: &Network) -> Option<Self> {
        // TODO: Parse Sapling full viewing key
        None
    }

    /// Accepts an encoded Sapling incoming viewing key to decode
    ///
    /// Returns a [`SaplingViewingKey::Ivk`] if successful, or None otherwise
    fn parse_incoming_viewing_key(_sapling_key: &str, _network: &Network) -> Option<Self> {
        // TODO: Parse Sapling incoming viewing key
        None
    }

    /// Accepts an encoded Sapling viewing key to decode
    ///
    /// Returns a [`SaplingViewingKey`] if successful, or None otherwise
    pub(super) fn parse(key: &str, network: &Network) -> Option<Self> {
        // TODO: Try types with prefixes first if some don't have prefixes?
        Self::parse_extended_full_viewing_key(key, network)
            .or_else(|| Self::parse_diversifiable_full_viewing_key(key, network))
            .or_else(|| Self::parse_full_viewing_key(key, network))
            .or_else(|| Self::parse_incoming_viewing_key(key, network))
    }
}

impl Network {
    /// Returns the human-readable prefix for an Zcash Sapling extended full viewing key
    /// for this network.
    pub fn sapling_efvk_hrp(&self) -> &'static str {
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
