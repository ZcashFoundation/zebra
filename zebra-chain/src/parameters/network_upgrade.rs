//! Network upgrade consensus parameters for Zcash.

use NetworkUpgrade::*;

use crate::block;
use crate::parameters::{Network, Network::*};

use std::collections::{BTreeMap, HashMap};
use std::ops::Bound::*;

use chrono::{DateTime, Duration, Utc};

/// A Zcash network upgrade.
///
/// Network upgrades can change the Zcash network protocol or consensus rules in
/// incompatible ways.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
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
    /// The Zcash protocol after the Nu5 upgrade.
    ///
    /// Note: Network Upgrade 5 includes the Orchard Shielded Protocol, and
    /// other changes. The Nu5 code name has not been chosen yet.
    Nu5,
}

/// Mainnet network upgrade activation heights.
///
/// This is actually a bijective map, but it is const, so we use a vector, and
/// do the uniqueness check in the unit tests.
pub(crate) const MAINNET_ACTIVATION_HEIGHTS: &[(block::Height, NetworkUpgrade)] = &[
    (block::Height(0), Genesis),
    (block::Height(1), BeforeOverwinter),
    (block::Height(347_500), Overwinter),
    (block::Height(419_200), Sapling),
    (block::Height(653_600), Blossom),
    (block::Height(903_000), Heartwood),
    (block::Height(1_046_400), Canopy),
    // TODO: Add Nu5 mainnet activation height
];

/// Testnet network upgrade activation heights.
///
/// This is actually a bijective map, but it is const, so we use a vector, and
/// do the uniqueness check in the unit tests.
pub(crate) const TESTNET_ACTIVATION_HEIGHTS: &[(block::Height, NetworkUpgrade)] = &[
    (block::Height(0), Genesis),
    (block::Height(1), BeforeOverwinter),
    (block::Height(207_500), Overwinter),
    (block::Height(280_000), Sapling),
    (block::Height(584_000), Blossom),
    (block::Height(903_800), Heartwood),
    (block::Height(1_028_500), Canopy),
    // TODO: Add Nu5 testnet activation height
];

/// The Consensus Branch Id, used to bind transactions and blocks to a
/// particular network upgrade.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq)]
pub struct ConsensusBranchId(u32);

impl From<ConsensusBranchId> for u32 {
    fn from(branch: ConsensusBranchId) -> u32 {
        branch.0
    }
}

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
    (Overwinter, ConsensusBranchId(0x5ba81b19)),
    (Sapling, ConsensusBranchId(0x76b809bb)),
    (Blossom, ConsensusBranchId(0x2bb40e60)),
    (Heartwood, ConsensusBranchId(0xf5b9230b)),
    (Canopy, ConsensusBranchId(0xe9ff75a6)),
    (Nu5, ConsensusBranchId(0xf919a198)),
];

/// The target block spacing before Blossom.
const PRE_BLOSSOM_POW_TARGET_SPACING: i64 = 150;

/// The target block spacing after Blossom activation.
const POST_BLOSSOM_POW_TARGET_SPACING: i64 = 75;

/// The averaging window for difficulty threshold arithmetic mean calculations.
///
/// `PoWAveragingWindow` in the Zcash specification.
pub const POW_AVERAGING_WINDOW: usize = 17;

/// The multiplier used to derive the testnet minimum difficulty block time gap
/// threshold.
///
/// Based on https://zips.z.cash/zip-0208#minimum-difficulty-blocks-on-the-test-network
const TESTNET_MINIMUM_DIFFICULTY_GAP_MULTIPLIER: i32 = 6;

/// The start height for the testnet minimum difficulty consensus rule.
///
/// Based on https://zips.z.cash/zip-0208#minimum-difficulty-blocks-on-the-test-network
const TESTNET_MINIMUM_DIFFICULTY_START_HEIGHT: block::Height = block::Height(299_188);

/// The activation height for the block maximum time rule on Testnet.
///
/// Part of the block header consensus rules in the Zcash specification at
/// https://zips.z.cash/protocol/protocol.pdf#blockheader
pub const TESTNET_MAX_TIME_START_HEIGHT: block::Height = block::Height(653_606);

impl NetworkUpgrade {
    /// Returns a BTreeMap of activation heights and network upgrades for
    /// `network`.
    ///
    /// If the activation height of a future upgrade is not known, that
    /// network upgrade does not appear in the list.
    ///
    /// This is actually a bijective map.
    pub(crate) fn activation_list(network: Network) -> BTreeMap<block::Height, NetworkUpgrade> {
        match network {
            Mainnet => MAINNET_ACTIVATION_HEIGHTS,
            Testnet => TESTNET_ACTIVATION_HEIGHTS,
        }
        .iter()
        .cloned()
        .collect()
    }

    /// Returns the current network upgrade for `network` and `height`.
    pub fn current(network: Network, height: block::Height) -> NetworkUpgrade {
        NetworkUpgrade::activation_list(network)
            .range(..=height)
            .map(|(_, nu)| *nu)
            .next_back()
            .expect("every height has a current network upgrade")
    }

    /// Returns the next network upgrade for `network` and `height`.
    ///
    /// Returns None if the next upgrade has not been implemented in Zebra
    /// yet.
    pub fn next(network: Network, height: block::Height) -> Option<NetworkUpgrade> {
        NetworkUpgrade::activation_list(network)
            .range((Excluded(height), Unbounded))
            .map(|(_, nu)| *nu)
            .next()
    }

    /// Returns the activation height for this network upgrade on `network`.
    ///
    /// Returns None if this network upgrade is a future upgrade, and its
    /// activation height has not been set yet.
    pub fn activation_height(&self, network: Network) -> Option<block::Height> {
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
        NetworkUpgrade::branch_id_list().get(self).cloned()
    }

    /// Returns the target block spacing for the network upgrade.
    ///
    /// Based on `PRE_BLOSSOM_POW_TARGET_SPACING` and
    /// `POST_BLOSSOM_POW_TARGET_SPACING` from the Zcash specification.
    pub fn target_spacing(&self) -> Duration {
        let spacing_seconds = match self {
            Genesis | BeforeOverwinter | Overwinter | Sapling => PRE_BLOSSOM_POW_TARGET_SPACING,
            Blossom | Heartwood | Canopy | Nu5 => POST_BLOSSOM_POW_TARGET_SPACING,
        };

        Duration::seconds(spacing_seconds)
    }

    /// Returns the target block spacing for `network` and `height`.
    ///
    /// See [`target_spacing()`] for details.
    pub fn target_spacing_for_height(network: Network, height: block::Height) -> Duration {
        NetworkUpgrade::current(network, height).target_spacing()
    }

    /// Returns the minimum difficulty block spacing for `network` and `height`.
    /// Returns `None` if the testnet minimum difficulty consensus rule is not active.
    ///
    /// Based on https://zips.z.cash/zip-0208#minimum-difficulty-blocks-on-the-test-network
    pub fn minimum_difficulty_spacing_for_height(
        network: Network,
        height: block::Height,
    ) -> Option<Duration> {
        match (network, height) {
            (Network::Testnet, height) if height < TESTNET_MINIMUM_DIFFICULTY_START_HEIGHT => None,
            (Network::Mainnet, _) => None,
            (Network::Testnet, _) => {
                let network_upgrade = NetworkUpgrade::current(network, height);
                Some(network_upgrade.target_spacing() * TESTNET_MINIMUM_DIFFICULTY_GAP_MULTIPLIER)
            }
        }
    }

    /// Returns true if the gap between `block_time` and `previous_block_time` is
    /// greater than the Testnet minimum difficulty time gap. This time gap
    /// depends on the `network` and `block_height`.
    ///
    /// Returns false on Mainnet, when `block_height` is less than the minimum
    /// difficulty start height, and when the time gap is too small.
    ///
    /// `block_time` can be less than, equal to, or greater than
    /// `previous_block_time`, because block times are provided by miners.
    ///
    /// Implements the Testnet minimum difficulty adjustment from ZIPs 205 and 208.
    ///
    /// Spec Note: Some parts of ZIPs 205 and 208 previously specified an incorrect
    /// check for the time gap. This function implements the correct "greater than"
    /// check.
    pub fn is_testnet_min_difficulty_block(
        network: Network,
        block_height: block::Height,
        block_time: DateTime<Utc>,
        previous_block_time: DateTime<Utc>,
    ) -> bool {
        let block_time_gap = block_time - previous_block_time;
        if let Some(min_difficulty_gap) =
            NetworkUpgrade::minimum_difficulty_spacing_for_height(network, block_height)
        {
            block_time_gap > min_difficulty_gap
        } else {
            false
        }
    }

    /// Returns the averaging window timespan for the network upgrade.
    ///
    /// `AveragingWindowTimespan` from the Zcash specification.
    pub fn averaging_window_timespan(&self) -> Duration {
        self.target_spacing() * (POW_AVERAGING_WINDOW as _)
    }

    /// Returns the averaging window timespan for `network` and `height`.
    ///
    /// See [`averaging_window_timespan()`] for details.
    pub fn averaging_window_timespan_for_height(
        network: Network,
        height: block::Height,
    ) -> Duration {
        NetworkUpgrade::current(network, height).averaging_window_timespan()
    }

    /// Returns true if the maximum block time rule is active for `network` and `height`.
    ///
    /// Always returns true if `network` is the Mainnet.
    /// If `network` is the Testnet, the `height` should be at least
    /// TESTNET_MAX_TIME_START_HEIGHT to return true.
    /// Returns false otherwise.
    ///
    /// Part of the consensus rules at https://zips.z.cash/protocol/protocol.pdf#blockheader
    pub fn is_max_block_time_enforced(network: Network, height: block::Height) -> bool {
        match network {
            Network::Mainnet => true,
            Network::Testnet => height >= TESTNET_MAX_TIME_START_HEIGHT,
        }
    }
    /// Returns the NetworkUpgrade given an u32 as ConsensusBranchId
    pub fn from_branch_id(branch_id: u32) -> Option<NetworkUpgrade> {
        CONSENSUS_BRANCH_IDS
            .iter()
            .find(|id| id.1 == ConsensusBranchId(branch_id))
            .map(|nu| nu.0)
    }
}

impl ConsensusBranchId {
    /// Returns the current consensus branch id for `network` and `height`.
    ///
    /// Returns None if the network has no branch id at this height.
    pub fn current(network: Network, height: block::Height) -> Option<ConsensusBranchId> {
        NetworkUpgrade::current(network, height).branch_id()
    }
}
