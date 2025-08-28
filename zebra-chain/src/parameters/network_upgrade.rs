//! Network upgrade consensus parameters for Zcash.

use NetworkUpgrade::*;

use crate::block;
use crate::parameters::{Network, Network::*};

use std::collections::{BTreeMap, HashMap};
use std::fmt;

use chrono::{DateTime, Duration, Utc};
use hex::{FromHex, ToHex};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// A list of network upgrades in the order that they must be activated.
const NETWORK_UPGRADES_IN_ORDER: &[NetworkUpgrade] = &[
    Genesis,
    BeforeOverwinter,
    Overwinter,
    Sapling,
    Blossom,
    Heartwood,
    Canopy,
    Nu5,
    Nu6,
    Nu6_1,
    #[cfg(any(test, feature = "zebra-test"))]
    Nu7,
];

/// A Zcash network upgrade.
///
/// Network upgrades change the Zcash network protocol or consensus rules. Note that they have no
/// designated codenames from NU5 onwards.
#[derive(Copy, Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize, Ord, PartialOrd)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
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
    /// The Zcash protocol after the NU5 upgrade.
    #[serde(rename = "NU5")]
    Nu5,
    /// The Zcash protocol after the NU6 upgrade.
    #[serde(rename = "NU6")]
    Nu6,
    /// The Zcash protocol after the NU6.1 upgrade.
    #[serde(rename = "NU6.1")]
    Nu6_1,
    /// The Zcash protocol after the NU7 upgrade.
    #[serde(rename = "NU7")]
    Nu7,

    #[cfg(zcash_unstable = "zfuture")]
    ZFuture,
}

impl TryFrom<u32> for NetworkUpgrade {
    type Error = crate::Error;

    fn try_from(branch_id: u32) -> Result<Self, Self::Error> {
        CONSENSUS_BRANCH_IDS
            .iter()
            .find(|id| id.1 == ConsensusBranchId(branch_id))
            .map(|nu| nu.0)
            .ok_or(Self::Error::InvalidConsensusBranchId)
    }
}

impl fmt::Display for NetworkUpgrade {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        // Same as the debug representation for now
        fmt::Debug::fmt(self, f)
    }
}

/// Mainnet network upgrade activation heights.
///
/// This is actually a bijective map, but it is const, so we use a vector, and
/// do the uniqueness check in the unit tests.
///
/// # Correctness
///
/// Don't use this directly; use NetworkUpgrade::activation_list() so that
/// we can switch to fake activation heights for some tests.
#[allow(unused)]
pub(super) const MAINNET_ACTIVATION_HEIGHTS: &[(block::Height, NetworkUpgrade)] = &[
    (block::Height(0), Genesis),
    (block::Height(1), BeforeOverwinter),
    (block::Height(347_500), Overwinter),
    (block::Height(419_200), Sapling),
    (block::Height(653_600), Blossom),
    (block::Height(903_000), Heartwood),
    (block::Height(1_046_400), Canopy),
    (block::Height(1_687_104), Nu5),
    (block::Height(2_726_400), Nu6),
];

/// The block height at which NU6.1 activates on the default Testnet.
// See NU6.1 Testnet activation height in zcashd:
// <https://github.com/zcash/zcash/blob/b65b008a7b334a2f7c2eaae1b028e011f2e21dd1/src/chainparams.cpp#L472>
pub const NU6_1_ACTIVATION_HEIGHT_TESTNET: block::Height = block::Height(3_536_500);

/// Testnet network upgrade activation heights.
///
/// This is actually a bijective map, but it is const, so we use a vector, and
/// do the uniqueness check in the unit tests.
///
/// # Correctness
///
/// Don't use this directly; use NetworkUpgrade::activation_list() so that
/// we can switch to fake activation heights for some tests.
#[allow(unused)]
pub(super) const TESTNET_ACTIVATION_HEIGHTS: &[(block::Height, NetworkUpgrade)] = &[
    (block::Height(0), Genesis),
    (block::Height(1), BeforeOverwinter),
    (block::Height(207_500), Overwinter),
    (block::Height(280_000), Sapling),
    (block::Height(584_000), Blossom),
    (block::Height(903_800), Heartwood),
    (block::Height(1_028_500), Canopy),
    (block::Height(1_842_420), Nu5),
    (block::Height(2_976_000), Nu6),
    (NU6_1_ACTIVATION_HEIGHT_TESTNET, Nu6_1),
];

/// The Consensus Branch Id, used to bind transactions and blocks to a
/// particular network upgrade.
#[derive(Copy, Clone, Debug, Default, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub struct ConsensusBranchId(pub(crate) u32);

impl ConsensusBranchId {
    /// Return the hash bytes in big-endian byte-order suitable for printing out byte by byte.
    ///
    /// Zebra displays consensus branch IDs in big-endian byte-order,
    /// following the convention set by zcashd.
    fn bytes_in_display_order(&self) -> [u8; 4] {
        self.0.to_be_bytes()
    }
}

impl From<ConsensusBranchId> for u32 {
    fn from(branch: ConsensusBranchId) -> u32 {
        branch.0
    }
}

impl From<u32> for ConsensusBranchId {
    fn from(branch: u32) -> Self {
        ConsensusBranchId(branch)
    }
}

impl ToHex for &ConsensusBranchId {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl ToHex for ConsensusBranchId {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl FromHex for ConsensusBranchId {
    type Error = <[u8; 4] as FromHex>::Error;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let branch = <[u8; 4]>::from_hex(hex)?;
        Ok(ConsensusBranchId(u32::from_be_bytes(branch)))
    }
}

impl fmt::Display for ConsensusBranchId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.encode_hex::<String>())
    }
}

impl TryFrom<ConsensusBranchId> for zcash_primitives::consensus::BranchId {
    type Error = crate::Error;

    fn try_from(id: ConsensusBranchId) -> Result<Self, Self::Error> {
        zcash_primitives::consensus::BranchId::try_from(u32::from(id))
            .map_err(|_| Self::Error::InvalidConsensusBranchId)
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
    (Nu5, ConsensusBranchId(0xc2d6d0b4)),
    (Nu6, ConsensusBranchId(0xc8e71055)),
    (Nu6_1, ConsensusBranchId(0x4dec4df0)),
    #[cfg(any(test, feature = "zebra-test"))]
    (Nu7, ConsensusBranchId(0x77190ad8)),
    #[cfg(zcash_unstable = "zfuture")]
    (ZFuture, ConsensusBranchId(0xffffffff)),
];

/// The target block spacing before Blossom.
const PRE_BLOSSOM_POW_TARGET_SPACING: i64 = 150;

/// The target block spacing after Blossom activation.
pub const POST_BLOSSOM_POW_TARGET_SPACING: u32 = 75;

/// The averaging window for difficulty threshold arithmetic mean calculations.
///
/// `PoWAveragingWindow` in the Zcash specification.
pub const POW_AVERAGING_WINDOW: usize = 17;

/// The multiplier used to derive the testnet minimum difficulty block time gap
/// threshold.
///
/// Based on <https://zips.z.cash/zip-0208#minimum-difficulty-blocks-on-the-test-network>
const TESTNET_MINIMUM_DIFFICULTY_GAP_MULTIPLIER: i32 = 6;

/// The start height for the testnet minimum difficulty consensus rule.
///
/// Based on <https://zips.z.cash/zip-0208#minimum-difficulty-blocks-on-the-test-network>
const TESTNET_MINIMUM_DIFFICULTY_START_HEIGHT: block::Height = block::Height(299_188);

/// The activation height for the block maximum time rule on Testnet.
///
/// Part of the block header consensus rules in the Zcash specification at
/// <https://zips.z.cash/protocol/protocol.pdf#blockheader>
pub const TESTNET_MAX_TIME_START_HEIGHT: block::Height = block::Height(653_606);

impl Network {
    /// Returns a map between activation heights and network upgrades for `network`,
    /// in ascending height order.
    ///
    /// If the activation height of a future upgrade is not known, that
    /// network upgrade does not appear in the list.
    ///
    /// This is actually a bijective map.
    ///
    /// Note: This skips implicit network upgrade activations, use [`Network::full_activation_list`]
    ///       to get an explicit list of all network upgrade activations.
    pub fn activation_list(&self) -> BTreeMap<block::Height, NetworkUpgrade> {
        match self {
            Mainnet => MAINNET_ACTIVATION_HEIGHTS.iter().cloned().collect(),
            Testnet(params) => params.activation_heights().clone(),
        }
    }

    /// Returns a vector of all implicit and explicit network upgrades for `network`,
    /// in ascending height order.
    pub fn full_activation_list(&self) -> Vec<(block::Height, NetworkUpgrade)> {
        NETWORK_UPGRADES_IN_ORDER
            .iter()
            .map_while(|&nu| Some((NetworkUpgrade::activation_height(&nu, self)?, nu)))
            .collect()
    }
}

impl NetworkUpgrade {
    /// Returns the current network upgrade and its activation height for `network` and `height`.
    pub fn current_with_activation_height(
        network: &Network,
        height: block::Height,
    ) -> (NetworkUpgrade, block::Height) {
        network
            .activation_list()
            .range(..=height)
            .map(|(&h, &nu)| (nu, h))
            .next_back()
            .expect("every height has a current network upgrade")
    }

    /// Returns the current network upgrade for `network` and `height`.
    pub fn current(network: &Network, height: block::Height) -> NetworkUpgrade {
        network
            .activation_list()
            .range(..=height)
            .map(|(_, nu)| *nu)
            .next_back()
            .expect("every height has a current network upgrade")
    }

    /// Returns the next expected network upgrade after this network upgrade.
    pub fn next_upgrade(self) -> Option<Self> {
        Self::iter().skip_while(|&nu| self != nu).nth(1)
    }

    /// Returns the previous network upgrade before this network upgrade.
    pub fn previous_upgrade(self) -> Option<Self> {
        Self::iter().rev().skip_while(|&nu| self != nu).nth(1)
    }

    /// Returns the next network upgrade for `network` and `height`.
    ///
    /// Returns None if the next upgrade has not been implemented in Zebra
    /// yet.
    #[cfg(test)]
    pub fn next(network: &Network, height: block::Height) -> Option<NetworkUpgrade> {
        use std::ops::Bound::*;

        network
            .activation_list()
            .range((Excluded(height), Unbounded))
            .map(|(_, nu)| *nu)
            .next()
    }

    /// Returns the activation height for this network upgrade on `network`, or
    ///
    /// Returns the activation height of the first network upgrade that follows
    /// this network upgrade if there is no activation height for this network upgrade
    /// such as on Regtest or a configured Testnet where multiple network upgrades have the
    /// same activation height, or if one is omitted when others that follow it are included.
    ///
    /// Returns None if this network upgrade is a future upgrade, and its
    /// activation height has not been set yet.
    ///
    /// Returns None if this network upgrade has not been configured on a Testnet or Regtest.
    pub fn activation_height(&self, network: &Network) -> Option<block::Height> {
        network
            .activation_list()
            .iter()
            .find(|(_, nu)| nu == &self)
            .map(|(height, _)| *height)
            .or_else(|| {
                self.next_upgrade()
                    .and_then(|next_nu| next_nu.activation_height(network))
            })
    }

    /// Returns `true` if `height` is the activation height of any network upgrade
    /// on `network`.
    ///
    /// Use [`NetworkUpgrade::activation_height`] to get the specific network
    /// upgrade.
    pub fn is_activation_height(network: &Network, height: block::Height) -> bool {
        network.activation_list().contains_key(&height)
    }

    /// Returns an unordered mapping between NetworkUpgrades and their ConsensusBranchIds.
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
    /// Based on [`PRE_BLOSSOM_POW_TARGET_SPACING`] and
    /// [`POST_BLOSSOM_POW_TARGET_SPACING`] from the Zcash specification.
    pub fn target_spacing(&self) -> Duration {
        let spacing_seconds = match self {
            Genesis | BeforeOverwinter | Overwinter | Sapling => PRE_BLOSSOM_POW_TARGET_SPACING,
            Blossom | Heartwood | Canopy | Nu5 | Nu6 | Nu6_1 | Nu7 => {
                POST_BLOSSOM_POW_TARGET_SPACING.into()
            }

            #[cfg(zcash_unstable = "zfuture")]
            ZFuture => POST_BLOSSOM_POW_TARGET_SPACING.into(),
        };

        Duration::seconds(spacing_seconds)
    }

    /// Returns the target block spacing for `network` and `height`.
    ///
    /// See [`NetworkUpgrade::target_spacing`] for details.
    pub fn target_spacing_for_height(network: &Network, height: block::Height) -> Duration {
        NetworkUpgrade::current(network, height).target_spacing()
    }

    /// Returns all the target block spacings for `network` and the heights where they start.
    pub fn target_spacings(
        network: &Network,
    ) -> impl Iterator<Item = (block::Height, Duration)> + '_ {
        [
            (NetworkUpgrade::Genesis, PRE_BLOSSOM_POW_TARGET_SPACING),
            (
                NetworkUpgrade::Blossom,
                POST_BLOSSOM_POW_TARGET_SPACING.into(),
            ),
        ]
        .into_iter()
        .filter_map(move |(upgrade, spacing_seconds)| {
            let activation_height = upgrade.activation_height(network)?;
            let target_spacing = Duration::seconds(spacing_seconds);
            Some((activation_height, target_spacing))
        })
    }

    /// Returns the minimum difficulty block spacing for `network` and `height`.
    /// Returns `None` if the testnet minimum difficulty consensus rule is not active.
    ///
    /// Based on <https://zips.z.cash/zip-0208#minimum-difficulty-blocks-on-the-test-network>
    pub fn minimum_difficulty_spacing_for_height(
        network: &Network,
        height: block::Height,
    ) -> Option<Duration> {
        match (network, height) {
            // TODO: Move `TESTNET_MINIMUM_DIFFICULTY_START_HEIGHT` to a field on testnet::Parameters (#8364)
            (Network::Testnet(_params), height)
                if height < TESTNET_MINIMUM_DIFFICULTY_START_HEIGHT =>
            {
                None
            }
            (Network::Mainnet, _) => None,
            (Network::Testnet(_params), _) => {
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
        network: &Network,
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
        self.target_spacing() * POW_AVERAGING_WINDOW.try_into().expect("fits in i32")
    }

    /// Returns the averaging window timespan for `network` and `height`.
    ///
    /// See [`NetworkUpgrade::averaging_window_timespan`] for details.
    pub fn averaging_window_timespan_for_height(
        network: &Network,
        height: block::Height,
    ) -> Duration {
        NetworkUpgrade::current(network, height).averaging_window_timespan()
    }

    /// Returns an iterator over [`NetworkUpgrade`] variants.
    pub fn iter() -> impl DoubleEndedIterator<Item = NetworkUpgrade> {
        NETWORK_UPGRADES_IN_ORDER.iter().copied()
    }
}

impl From<zcash_protocol::consensus::NetworkUpgrade> for NetworkUpgrade {
    fn from(nu: zcash_protocol::consensus::NetworkUpgrade) -> Self {
        match nu {
            zcash_protocol::consensus::NetworkUpgrade::Overwinter => Self::Overwinter,
            zcash_protocol::consensus::NetworkUpgrade::Sapling => Self::Sapling,
            zcash_protocol::consensus::NetworkUpgrade::Blossom => Self::Blossom,
            zcash_protocol::consensus::NetworkUpgrade::Heartwood => Self::Heartwood,
            zcash_protocol::consensus::NetworkUpgrade::Canopy => Self::Canopy,
            zcash_protocol::consensus::NetworkUpgrade::Nu5 => Self::Nu5,
            zcash_protocol::consensus::NetworkUpgrade::Nu6 => Self::Nu6,
            zcash_protocol::consensus::NetworkUpgrade::Nu6_1 => Self::Nu6_1,
            #[cfg(zcash_unstable = "nu7")]
            zcash_protocol::consensus::NetworkUpgrade::Nu7 => Self::Nu7,
            #[cfg(zcash_unstable = "zfuture")]
            zcash_protocol::consensus::NetworkUpgrade::ZFuture => Self::ZFuture,
        }
    }
}

impl ConsensusBranchId {
    /// The value used by `zcashd` RPCs for missing consensus branch IDs.
    ///
    /// # Consensus
    ///
    /// This value must only be used in RPCs.
    ///
    /// The consensus rules handle missing branch IDs by rejecting blocks and transactions,
    /// so this substitute value must not be used in consensus-critical code.
    pub const RPC_MISSING_ID: ConsensusBranchId = ConsensusBranchId(0);

    /// Returns the current consensus branch id for `network` and `height`.
    ///
    /// Returns None if the network has no branch id at this height.
    pub fn current(network: &Network, height: block::Height) -> Option<ConsensusBranchId> {
        NetworkUpgrade::current(network, height).branch_id()
    }
}
