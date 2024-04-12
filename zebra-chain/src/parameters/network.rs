//! Consensus parameters for each Zcash network.

use std::{fmt, str::FromStr, sync::Arc};

use thiserror::Error;

use zcash_primitives::{consensus as zp_consensus, constants as zp_constants};

use crate::{
    block::{self, Height, HeightDiff},
    parameters::NetworkUpgrade,
};

pub mod testnet;

#[cfg(test)]
mod tests;

/// The ZIP-212 grace period length after the Canopy activation height.
///
/// # Consensus
///
/// ZIP-212 requires Zcash nodes to validate that Sapling spends and Orchard actions follows a
/// specific plaintext format after Canopy's activation.
///
/// > [Heartwood onward] All Sapling and Orchard outputs in coinbase transactions MUST decrypt to a
/// > note plaintext , i.e. the procedure in § 4.19.3 ‘Decryption using a Full Viewing Key (Sapling
/// > and Orchard)’ on p. 67 does not return ⊥, using a sequence of 32 zero bytes as the outgoing
/// > viewing key . (This implies that before Canopy activation, Sapling outputs of a coinbase
/// > transaction MUST have note plaintext lead byte equal to 0x01.)
///
/// > [Canopy onward] Any Sapling or Orchard output of a coinbase transaction decrypted to a note
/// > plaintext according to the preceding rule MUST have note plaintext lead byte equal to 0x02.
/// > (This applies even during the “grace period” specified in [ZIP-212].)
///
/// <https://zips.z.cash/protocol/protocol.pdf#txnencodingandconsensus>
///
/// Wallets have a grace period of 32,256 blocks after Canopy's activation to validate those blocks,
/// but nodes do not.
///
/// > There is a "grace period" of 32256 blocks starting from the block at which this ZIP activates,
/// > during which note plaintexts with lead byte 0x01 MUST still be accepted [by wallets].
/// >
/// > Let ActivationHeight be the activation height of this ZIP, and let GracePeriodEndHeight be
/// > ActivationHeight + 32256.
///
/// <https://zips.z.cash/zip-0212#changes-to-the-process-of-receiving-sapling-or-orchard-notes>
///
/// Zebra uses `librustzcash` to validate that rule, but it won't validate it during the grace
/// period. Therefore Zebra must validate those blocks during the grace period using checkpoints.
/// Therefore the mandatory checkpoint height ([`Network::mandatory_checkpoint_height`]) must be
/// after the grace period.
const ZIP_212_GRACE_PERIOD_DURATION: HeightDiff = 32_256;

/// An enum describing the kind of network, whether it's the production mainnet or a testnet.
// Note: The order of these variants is important for correct bincode (de)serialization
//       of history trees in the db format.
// TODO: Replace bincode (de)serialization of `HistoryTreeParts` in a db format upgrade?
#[derive(Copy, Clone, Default, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum NetworkKind {
    /// The production mainnet.
    #[default]
    Mainnet,

    /// A test network.
    Testnet,

    /// Regtest mode, not yet implemented
    // TODO: Add `new_regtest()` and `is_regtest` methods on `Network`.
    Regtest,
}

impl From<Network> for NetworkKind {
    fn from(network: Network) -> Self {
        network.kind()
    }
}

/// An enum describing the possible network choices.
#[derive(Clone, Debug, Default, Eq, PartialEq, Hash, Serialize)]
#[serde(into = "NetworkKind")]
pub enum Network {
    /// The production mainnet.
    #[default]
    Mainnet,

    /// A test network such as the default public testnet,
    /// a configured testnet, or Regtest.
    Testnet(Arc<testnet::Parameters>),
}

impl NetworkKind {
    /// Returns the human-readable prefix for Base58Check-encoded transparent
    /// pay-to-public-key-hash payment addresses for the network.
    pub fn b58_pubkey_address_prefix(self) -> [u8; 2] {
        match self {
            Self::Mainnet => zp_constants::mainnet::B58_PUBKEY_ADDRESS_PREFIX,
            Self::Testnet | Self::Regtest => zp_constants::testnet::B58_PUBKEY_ADDRESS_PREFIX,
        }
    }

    /// Returns the human-readable prefix for Base58Check-encoded transparent pay-to-script-hash
    /// payment addresses for the network.
    pub fn b58_script_address_prefix(self) -> [u8; 2] {
        match self {
            Self::Mainnet => zp_constants::mainnet::B58_SCRIPT_ADDRESS_PREFIX,
            Self::Testnet | Self::Regtest => zp_constants::testnet::B58_SCRIPT_ADDRESS_PREFIX,
        }
    }

    /// Return the network name as defined in
    /// [BIP70](https://github.com/bitcoin/bips/blob/master/bip-0070.mediawiki#paymentdetailspaymentrequest)
    pub fn bip70_network_name(&self) -> String {
        if *self == Self::Mainnet {
            "main".to_string()
        } else {
            "test".to_string()
        }
    }
}

impl From<NetworkKind> for &'static str {
    fn from(network: NetworkKind) -> &'static str {
        // These should be different from the `Display` impl for `Network` so that its lowercase form
        // can't be parsed as the default Testnet in the `Network` `FromStr` impl, it's easy to
        // distinguish them in logs, and so it's generally harder to confuse the two.
        match network {
            NetworkKind::Mainnet => "MainnetKind",
            NetworkKind::Testnet => "TestnetKind",
            NetworkKind::Regtest => "RegtestKind",
        }
    }
}

impl fmt::Display for NetworkKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str((*self).into())
    }
}

impl From<&Network> for &'static str {
    fn from(network: &Network) -> &'static str {
        match network {
            Network::Mainnet => "Mainnet",
            // TODO:
            // - Add a `name` field to use here instead of checking `is_default_testnet()`
            // - zcashd calls the Regtest cache dir 'regtest' (#8327).
            Network::Testnet(params) if params.is_default_testnet() => "Testnet",
            Network::Testnet(_params) => "UnknownTestnet",
        }
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.into())
    }
}

impl Network {
    /// Creates a new [`Network::Testnet`] with the default Testnet [`testnet::Parameters`].
    pub fn new_default_testnet() -> Self {
        Self::Testnet(Arc::new(testnet::Parameters::default()))
    }

    /// Creates a new configured [`Network::Testnet`] with the provided Testnet [`testnet::Parameters`].
    pub fn new_configured_testnet(params: testnet::Parameters) -> Self {
        Self::Testnet(Arc::new(params))
    }

    /// Returns true if the network is the default Testnet, or false otherwise.
    pub fn is_default_testnet(&self) -> bool {
        if let Self::Testnet(params) = self {
            params.is_default_testnet()
        } else {
            false
        }
    }

    /// Returns the [`NetworkKind`] for this network.
    pub fn kind(&self) -> NetworkKind {
        match self {
            Network::Mainnet => NetworkKind::Mainnet,
            // TODO: Return `NetworkKind::Regtest` if the parameters match the default Regtest params
            Network::Testnet(_) => NetworkKind::Testnet,
        }
    }

    /// Returns an iterator over [`Network`] variants.
    pub fn iter() -> impl Iterator<Item = Self> {
        // TODO: Use default values of `Testnet` variant when adding fields for #7845.
        [Self::Mainnet, Self::new_default_testnet()].into_iter()
    }

    /// Returns true if the maximum block time rule is active for `network` and `height`.
    ///
    /// Always returns true if `network` is the Mainnet.
    /// If `network` is the Testnet, the `height` should be at least
    /// TESTNET_MAX_TIME_START_HEIGHT to return true.
    /// Returns false otherwise.
    ///
    /// Part of the consensus rules at <https://zips.z.cash/protocol/protocol.pdf#blockheader>
    pub fn is_max_block_time_enforced(&self, height: block::Height) -> bool {
        match self {
            Network::Mainnet => true,
            // TODO: Move `TESTNET_MAX_TIME_START_HEIGHT` to a field on testnet::Parameters (#8364)
            Network::Testnet(_params) => height >= super::TESTNET_MAX_TIME_START_HEIGHT,
        }
    }

    /// Get the default port associated to this network.
    pub fn default_port(&self) -> u16 {
        match self {
            Network::Mainnet => 8233,
            // TODO: Add a `default_port` field to `testnet::Parameters` to return here. (zcashd uses 18344 for Regtest)
            Network::Testnet(_params) => 18233,
        }
    }

    /// Get the mandatory minimum checkpoint height for this network.
    ///
    /// Mandatory checkpoints are a Zebra-specific feature.
    /// If a Zcash consensus rule only applies before the mandatory checkpoint,
    /// Zebra can skip validation of that rule.
    pub fn mandatory_checkpoint_height(&self) -> Height {
        // Currently this is after the ZIP-212 grace period.
        //
        // See the `ZIP_212_GRACE_PERIOD_DURATION` documentation for more information.

        let canopy_activation = NetworkUpgrade::Canopy
            .activation_height(self)
            .expect("Canopy activation height must be present for both networks");

        (canopy_activation + ZIP_212_GRACE_PERIOD_DURATION)
            .expect("ZIP-212 grace period ends at a valid block height")
    }

    /// Return the network name as defined in
    /// [BIP70](https://github.com/bitcoin/bips/blob/master/bip-0070.mediawiki#paymentdetailspaymentrequest)
    pub fn bip70_network_name(&self) -> String {
        self.kind().bip70_network_name()
    }

    /// Return the lowercase network name.
    pub fn lowercase_name(&self) -> String {
        self.to_string().to_ascii_lowercase()
    }

    /// Returns `true` if this network is a testing network.
    pub fn is_a_test_network(&self) -> bool {
        *self != Network::Mainnet
    }

    /// Returns the Sapling activation height for this network.
    pub fn sapling_activation_height(&self) -> Height {
        super::NetworkUpgrade::Sapling
            .activation_height(self)
            .expect("Sapling activation height needs to be set")
    }
}

// This is used for parsing a command-line argument for the `TipHeight` command in zebrad.
impl FromStr for Network {
    type Err = InvalidNetworkError;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        match string.to_lowercase().as_str() {
            "mainnet" => Ok(Network::Mainnet),
            "testnet" => Ok(Network::new_default_testnet()),
            _ => Err(InvalidNetworkError(string.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Error)]
#[error("Invalid network: {0}")]
pub struct InvalidNetworkError(String);

impl zp_consensus::Parameters for Network {
    fn activation_height(
        &self,
        nu: zcash_primitives::consensus::NetworkUpgrade,
    ) -> Option<zcash_primitives::consensus::BlockHeight> {
        let target_nu = match nu {
            zp_consensus::NetworkUpgrade::Overwinter => NetworkUpgrade::Overwinter,
            zp_consensus::NetworkUpgrade::Sapling => NetworkUpgrade::Sapling,
            zp_consensus::NetworkUpgrade::Blossom => NetworkUpgrade::Blossom,
            zp_consensus::NetworkUpgrade::Heartwood => NetworkUpgrade::Heartwood,
            zp_consensus::NetworkUpgrade::Canopy => NetworkUpgrade::Canopy,
            zp_consensus::NetworkUpgrade::Nu5 => NetworkUpgrade::Nu5,
        };

        // Heights are hard-coded below Height::MAX or checked when the config is parsed.
        target_nu
            .activation_height(self)
            .map(|Height(h)| zp_consensus::BlockHeight::from_u32(h))
    }

    fn coin_type(&self) -> u32 {
        match self {
            Network::Mainnet => zp_constants::mainnet::COIN_TYPE,
            // The regtest cointype reuses the testnet cointype,
            // See <https://github.com/satoshilabs/slips/blob/master/slip-0044.md>
            Network::Testnet(_) => zp_constants::testnet::COIN_TYPE,
        }
    }

    fn address_network(&self) -> Option<zcash_address::Network> {
        match self {
            Network::Mainnet => Some(zcash_address::Network::Main),
            Network::Testnet(params) if params.is_default_testnet() => {
                Some(zcash_address::Network::Test)
            }
            _other => None,
        }
    }

    fn hrp_sapling_extended_spending_key(&self) -> &str {
        match self {
            Network::Mainnet => zp_constants::mainnet::HRP_SAPLING_EXTENDED_SPENDING_KEY,
            Network::Testnet(params) if params.is_default_testnet() => {
                zp_constants::testnet::HRP_SAPLING_EXTENDED_SPENDING_KEY
            }
            // TODO: Add this as a field to testnet params
            _other => "secret-extended-key-unknown-test",
        }
    }

    fn hrp_sapling_extended_full_viewing_key(&self) -> &str {
        match self {
            Network::Mainnet => zp_constants::mainnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY,
            Network::Testnet(params) if params.is_default_testnet() => {
                zp_constants::testnet::HRP_SAPLING_EXTENDED_FULL_VIEWING_KEY
            }
            // TODO: Add this as a field to testnet params
            _other => "zxviewunknowntestsapling",
        }
    }

    fn hrp_sapling_payment_address(&self) -> &str {
        match self {
            Network::Mainnet => zp_constants::mainnet::HRP_SAPLING_PAYMENT_ADDRESS,
            Network::Testnet(params) if params.is_default_testnet() => {
                zp_constants::testnet::HRP_SAPLING_PAYMENT_ADDRESS
            }
            // TODO: Add this as a field to testnet params
            _other => "zunknowntestsapling",
        }
    }

    fn b58_pubkey_address_prefix(&self) -> [u8; 2] {
        self.kind().b58_pubkey_address_prefix()
    }

    fn b58_script_address_prefix(&self) -> [u8; 2] {
        self.kind().b58_script_address_prefix()
    }
}
