//! Defines types and implements methods for parsing Sapling viewing keys and converting them to `zebra-chain` types

use group::GroupEncoding;
use jubjub::ExtendedPoint;
use zcash_client_backend::encoding::decode_extended_full_viewing_key;
use zcash_primitives::{
    constants::*,
    sapling::keys::{FullViewingKey as SaplingFvk, SaplingIvk, ViewingKey as SaplingVk},
    zip32::DiversifiableFullViewingKey as SaplingDfvk,
};

use crate::parameters::Network;

/// A Zcash Sapling viewing key
#[derive(Debug, Clone)]
pub enum SaplingViewingKey {
    /// A viewing key for Sapling
    Vk(Box<SaplingVk>),

    /// An incoming viewing key for Sapling
    Ivk(Box<SaplingIvk>),

    /// A full viewing key for Sapling
    Fvk(Box<SaplingFvk>),

    /// A diversifiable full viewing key for Sapling
    Dfvk(Box<SaplingDfvk>),
}

/// Accepts a Sapling viewing key, [`SaplingVk`]
///
/// Returns its byte representation.
fn viewing_key_to_bytes(SaplingVk { ak, nk }: &SaplingVk) -> Vec<u8> {
    ExtendedPoint::from(*ak)
        .to_bytes()
        .into_iter()
        .chain(ExtendedPoint::from(nk.0).to_bytes())
        .collect()
}

impl SaplingViewingKey {
    /// Returns an encoded byte representation of the Sapling viewing key
    pub(super) fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Vk(vk) => viewing_key_to_bytes(vk),
            Self::Ivk(ivk) => ivk.to_repr().to_vec(),
            Self::Fvk(fvk) => fvk.to_bytes().to_vec(),
            Self::Dfvk(dfvk) => dfvk.to_bytes().to_vec(),
        }
    }

    /// Accepts an encoded Sapling extended full viewing key to decode
    ///
    /// Returns a [`SaplingViewingKey::Dfvk`] if successful, or None otherwise
    fn parse_extended_full_viewing_key(sapling_key: &str, network: Network) -> Option<Self> {
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
    fn parse_diversifiable_full_viewing_key(_sapling_key: &str, _network: Network) -> Option<Self> {
        // TODO: Parse Sapling diversifiable full viewing key
        None
    }

    /// Accepts an encoded Sapling full viewing key to decode
    ///
    /// Returns a [`SaplingViewingKey::Fvk`] if successful, or None otherwise
    fn parse_full_viewing_key(_sapling_key: &str, _network: Network) -> Option<Self> {
        // TODO: Parse Sapling full viewing key
        None
    }

    /// Accepts an encoded Sapling incoming viewing key to decode
    ///
    /// Returns a [`SaplingViewingKey::Ivk`] if successful, or None otherwise
    fn parse_incoming_viewing_key(_sapling_key: &str, _network: Network) -> Option<Self> {
        // TODO: Parse Sapling incoming viewing key
        None
    }

    /// Accepts an encoded Sapling viewing key to decode
    ///
    /// Returns a [`SaplingViewingKey::Vk`] if successful, or None otherwise
    fn parse_viewing_key(_sapling_key: &str, _network: Network) -> Option<Self> {
        // TODO: Parse Sapling viewing key
        None
    }

    /// Accepts an encoded Sapling viewing key to decode
    ///
    /// Returns a [`SaplingViewingKey`] if successful, or None otherwise
    pub(super) fn parse(key: &str, network: Network) -> Option<Self> {
        // TODO: Try types with prefixes first if some don't have prefixes?
        Self::parse_extended_full_viewing_key(key, network)
            .or_else(|| Self::parse_diversifiable_full_viewing_key(key, network))
            .or_else(|| Self::parse_full_viewing_key(key, network))
            .or_else(|| Self::parse_viewing_key(key, network))
            .or_else(|| Self::parse_incoming_viewing_key(key, network))
    }
}

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
