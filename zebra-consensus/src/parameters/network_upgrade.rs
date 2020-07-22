//! Network upgrade consensus parameters for Zcash.

use NetworkUpgrade::*;

use std::collections::BTreeMap;
use std::ops::Bound::*;

use zebra_chain::types::BlockHeight;
use zebra_chain::{Network, Network::*};

/// A Zcash network protocol upgrade.
//
// TODO: are new network upgrades a breaking change, or should we make this
//       enum non-exhaustive?
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum NetworkUpgrade {
    /// The Zcash protocol before the Overwinter upgrade.
    ///
    /// We avoid using `Sprout`, because the specification says that Sprout
    /// is the name of the pre-Sapling protocol, before and after Overwinter.
    BeforeOverwinter,
    /// The Zcash protocol after the Overwinter upgrade.
    Overwinter,
    /// The Zcash protocol after the Sapling upgrade.
    Sapling,
    /// The Zcash protocol after the Blossom upgrade.
    Blossom,
    /// The Zcash protocol after the Heartwood upgrade.
    Heartwood,
    /// The Zcash protocol after the Canopy upgrade.
    Canopy,
}

/// Mainnet network upgrade activation heights.
///
/// This is actually a bijective map, but it is const, so we use a vector, and
/// do the uniqueness check in the unit tests.
pub(crate) const MAINNET_ACTIVATION_HEIGHTS: &[(BlockHeight, NetworkUpgrade)] = &[
    (BlockHeight(0), BeforeOverwinter),
    (BlockHeight(347_500), Overwinter),
    (BlockHeight(419_200), Sapling),
    (BlockHeight(653_600), Blossom),
    (BlockHeight(903_000), Heartwood),
    (BlockHeight(1_046_400), Canopy),
];

/// Testnet network upgrade activation heights.
///
/// This is actually a bijective map, but it is const, so we use a vector, and
/// do the uniqueness check in the unit tests.
pub(crate) const TESTNET_ACTIVATION_HEIGHTS: &[(BlockHeight, NetworkUpgrade)] = &[
    (BlockHeight(0), BeforeOverwinter),
    (BlockHeight(207_500), Overwinter),
    (BlockHeight(280_000), Sapling),
    (BlockHeight(584_000), Blossom),
    (BlockHeight(903_800), Heartwood),
    // As of 21 July 2020, the Canopy testnet height has not been decided.
    // See ZIP 251 for updates.
];

impl NetworkUpgrade {
    /// Returns a BTreeMap of activation heights and network upgrades for
    /// `network`.
    ///
    /// If the activation height of a future upgrade is not known, that
    /// network upgrade does not appear in the list.
    ///
    /// This is actually a bijective map.
    pub(crate) fn activation_list(network: Network) -> BTreeMap<BlockHeight, NetworkUpgrade> {
        match network {
            Mainnet => MAINNET_ACTIVATION_HEIGHTS,
            Testnet => TESTNET_ACTIVATION_HEIGHTS,
        }
        .iter()
        .cloned()
        .collect()
    }

    /// Returns the current network upgrade for `network` and `height`.
    pub fn current(network: Network, height: BlockHeight) -> NetworkUpgrade {
        NetworkUpgrade::activation_list(network)
            .range(..=height)
            .map(|(_, nu)| *nu)
            .next_back()
            .expect("every height has a current network upgrade")
    }

    /// Returns the next network upgrade for `network` and `height`.
    ///
    /// Returns None if the name of the next upgrade has not been decided yet.
    pub fn next(network: Network, height: BlockHeight) -> Option<NetworkUpgrade> {
        NetworkUpgrade::activation_list(network)
            .range((Excluded(height), Unbounded))
            .map(|(_, nu)| *nu)
            .next()
    }

    /// Returns the activation height for this network upgrade on `network`.
    ///
    /// Returns None if this network upgrade is a future upgrade, and its
    /// activation height has not been set yet.
    pub fn activation_height(&self, network: Network) -> Option<BlockHeight> {
        NetworkUpgrade::activation_list(network)
            .iter()
            .filter(|(_, nu)| nu == &self)
            .map(|(height, _)| *height)
            .next()
    }
}
