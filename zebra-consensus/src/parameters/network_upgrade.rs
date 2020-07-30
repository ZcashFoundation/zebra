//! Network upgrade consensus parameters for Zcash.

use NetworkUpgrade::*;

use std::collections::{BTreeMap, HashMap};
use std::ops::Bound::*;

use zebra_chain::types::BlockHeight;
use zebra_chain::{Network, Network::*};

/// A Zcash network upgrade.
///
/// Network upgrades can change the Zcash network protocol or consensus rules in
/// incompatible ways.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub enum NetworkUpgrade {
    /// The Zcash protocol for a Genesis block.
    ///
    /// Zcash genesis blocks use a different set of consensus rules from
    /// other BeforeOverwinter blocks, so we treat them like a separate network
    /// upgrade.
    Genesis,
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
    (BlockHeight(0), Genesis),
    (BlockHeight(1), BeforeOverwinter),
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
    (BlockHeight(0), Genesis),
    (BlockHeight(1), BeforeOverwinter),
    (BlockHeight(207_500), Overwinter),
    (BlockHeight(280_000), Sapling),
    (BlockHeight(584_000), Blossom),
    (BlockHeight(903_800), Heartwood),
    // As of 27 July 2020, the Canopy testnet height is under final review.
    // See ZIP 251 for any updates.
    (BlockHeight(1_028_500), Canopy),
];

/// The Consensus Branch Id, used to bind transactions and blocks to a
/// particular network upgrade.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct ConsensusBranchId(u32);

/// Network Upgrade Consensus Branch Ids.
///
/// Branch ids are the same for mainnet and testnet. If there is a testnet
/// rollback after a bug, the branch id changes.
///
/// Branch ids were introduced in the Overwinter upgrade, so there are no
/// Genesis or BeforeOverwinter branch ids.
///
/// This is actually a bijective map, but it is const, so we use a vector, and
/// do the uniqueness check in the unit tests.
pub(crate) const CONSENSUS_BRANCH_IDS: &[(NetworkUpgrade, ConsensusBranchId)] = &[
    // TODO(teor): byte order?
    (Overwinter, ConsensusBranchId(0x5ba81b19)),
    (Sapling, ConsensusBranchId(0x76b809bb)),
    (Blossom, ConsensusBranchId(0x2bb40e60)),
    (Heartwood, ConsensusBranchId(0xf5b9230b)),
    // As of 21 July 2020. Could change before mainnet activation.
    // See ZIP 251 for updates.
    (Canopy, ConsensusBranchId(0xe9ff75a6)),
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

    /// Returns a BTreeMap of NetworkUpgrades and their ConsensusBranchIds.
    ///
    /// Branch ids are the same for mainnet and testnet.
    ///
    /// If network upgrade does not have a branch id, that network upgrade does
    /// not appear in the list.
    ///
    /// This is actually a bijective map.
    pub(crate) fn branch_id_list() -> HashMap<NetworkUpgrade, ConsensusBranchId> {
        CONSENSUS_BRANCH_IDS.iter().cloned().collect()
    }

    /// Returns the consensus branch id for this network upgrade.
    ///
    /// Returns None if this network upgrade has no consensus branch id.
    pub fn branch_id(&self) -> Option<ConsensusBranchId> {
        NetworkUpgrade::branch_id_list().get(&self).cloned()
    }
}

impl ConsensusBranchId {
    /// Returns the current consensus branch id for `network` and `height`.
    ///
    /// Returns None if the network has no branch id at this height.
    pub fn current(network: Network, height: BlockHeight) -> Option<ConsensusBranchId> {
        NetworkUpgrade::current(network, height).branch_id()
    }
}
