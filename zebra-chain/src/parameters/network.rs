//! Consensus parameters for each Zcash network.

use std::{fmt, str::FromStr, sync::Arc};

use thiserror::Error;

use crate::{
    amount::{Amount, NonNegative},
    block::{self, Height},
    parameters::NetworkUpgrade,
    transparent,
};
use zcash_protocol::constants::{mainnet as mainnet_constants, testnet as testnet_constants};

mod error;
pub mod magic;
pub mod subsidy;
pub mod testnet;

#[cfg(test)]
mod tests;

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

    /// Regtest mode
    Regtest,
}

impl From<Network> for NetworkKind {
    fn from(net: Network) -> Self {
        NetworkKind::from(&net)
    }
}

impl From<&Network> for NetworkKind {
    fn from(net: &Network) -> Self {
        net.kind()
    }
}

/// An enum describing the possible network choices.
#[derive(Clone, Default, Eq, PartialEq, Serialize)]
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
            Self::Mainnet => mainnet_constants::B58_PUBKEY_ADDRESS_PREFIX,
            Self::Testnet | Self::Regtest => {
                testnet_constants::B58_PUBKEY_ADDRESS_PREFIX
            }
        }
    }

    /// Returns the human-readable prefix for Base58Check-encoded transparent pay-to-script-hash
    /// payment addresses for the network.
    pub fn b58_script_address_prefix(self) -> [u8; 2] {
        match self {
            Self::Mainnet => mainnet_constants::B58_SCRIPT_ADDRESS_PREFIX,
            Self::Testnet | Self::Regtest => {
                testnet_constants::B58_SCRIPT_ADDRESS_PREFIX
            }
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

    /// Returns the 2 bytes prefix for Bech32m-encoded transparent TEX
    /// payment addresses for the network as defined in [ZIP-320](https://zips.z.cash/zip-0320.html).
    pub fn tex_address_prefix(self) -> [u8; 2] {
        // TODO: Add this bytes to `zcash_primitives::constants`?
        match self {
            Self::Mainnet => [0x1c, 0xb8],
            Self::Testnet | Self::Regtest => [0x1d, 0x25],
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

impl<'a> From<&'a Network> for &'a str {
    fn from(network: &'a Network) -> &'a str {
        match network {
            Network::Mainnet => "Mainnet",
            Network::Testnet(params) => params.network_name(),
        }
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.into())
    }
}

impl std::fmt::Debug for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mainnet => write!(f, "{self}"),
            Self::Testnet(params) if params.is_regtest() => f
                .debug_struct("Regtest")
                .field("activation_heights", params.activation_heights())
                .field("funding_streams", params.funding_streams())
                .field("lockbox_disbursements", &params.lockbox_disbursements())
                .field("checkpoints", &params.checkpoints())
                .finish(),
            Self::Testnet(params) if params.is_default_testnet() => {
                write!(f, "{self}")
            }
            Self::Testnet(params) => f.debug_tuple("ConfiguredTestnet").field(params).finish(),
        }
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

    /// Creates a new [`Network::Testnet`] with `Regtest` parameters and the provided network upgrade activation heights.
    pub fn new_regtest(params: testnet::RegtestParameters) -> Self {
        Self::new_configured_testnet(
            testnet::Parameters::new_regtest(params)
                .expect("regtest parameters should always be valid"),
        )
    }

    /// Returns true if the network is the default Testnet, or false otherwise.
    pub fn is_default_testnet(&self) -> bool {
        if let Self::Testnet(params) = self {
            params.is_default_testnet()
        } else {
            false
        }
    }

    /// Returns true if the network is Regtest, or false otherwise.
    pub fn is_regtest(&self) -> bool {
        if let Self::Testnet(params) = self {
            params.is_regtest()
        } else {
            false
        }
    }

    /// Returns the [`NetworkKind`] for this network.
    pub fn kind(&self) -> NetworkKind {
        match self {
            Network::Mainnet => NetworkKind::Mainnet,
            Network::Testnet(params) if params.is_regtest() => NetworkKind::Regtest,
            Network::Testnet(_) => NetworkKind::Testnet,
        }
    }

    /// Returns [`NetworkKind::Testnet`] on Testnet and Regtest, or [`NetworkKind::Mainnet`] on Mainnet.
    ///
    /// This is used for transparent addresses, as the address prefix is the same on Regtest as it is on Testnet.
    pub fn t_addr_kind(&self) -> NetworkKind {
        match self {
            Network::Mainnet => NetworkKind::Mainnet,
            Network::Testnet(_) => NetworkKind::Testnet,
        }
    }

    /// Returns an iterator over [`Network`] variants.
    pub fn iter() -> impl Iterator<Item = Self> {
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
    /// This is necessary because Zebra can't fully validate the blocks prior to Canopy.
    // TODO:
    // - Support constructing pre-Canopy coinbase tx and block templates and return `Height::MAX` instead of panicking
    //   when Canopy activation height is `None` (#8434)
    pub fn mandatory_checkpoint_height(&self) -> Height {
        // Currently this is just before Canopy activation
        NetworkUpgrade::Canopy
            .activation_height(self)
            .expect("Canopy activation height must be present on all networks")
            .previous()
            .expect("Canopy activation height must be above min height")
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
    // TODO: Return an `Option` here now that network upgrade activation heights are configurable on Regtest and custom Testnets
    pub fn sapling_activation_height(&self) -> Height {
        super::NetworkUpgrade::Sapling
            .activation_height(self)
            .expect("Sapling activation height needs to be set")
    }

    /// Returns the expected total value of the sum of all NU6.1 one-time lockbox disbursement output values for this network at
    /// the provided height.
    pub fn lockbox_disbursement_total_amount(&self, height: Height) -> Amount<NonNegative> {
        if Some(height) != NetworkUpgrade::Nu6_1.activation_height(self) {
            return Amount::zero();
        };

        match self {
            Self::Mainnet => {
                subsidy::constants::mainnet::EXPECTED_NU6_1_LOCKBOX_DISBURSEMENTS_TOTAL
            }
            Self::Testnet(params) if params.is_default_testnet() => {
                subsidy::constants::testnet::EXPECTED_NU6_1_LOCKBOX_DISBURSEMENTS_TOTAL
            }
            Self::Testnet(params) => params.lockbox_disbursement_total_amount(),
        }
    }

    /// Returns the expected NU6.1 lockbox disbursement outputs for this network at the provided height.
    pub fn lockbox_disbursements(
        &self,
        height: Height,
    ) -> Vec<(transparent::Address, Amount<NonNegative>)> {
        if Some(height) != NetworkUpgrade::Nu6_1.activation_height(self) {
            return Vec::new();
        };

        let expected_lockbox_disbursements = match self {
            Self::Mainnet => subsidy::constants::mainnet::NU6_1_LOCKBOX_DISBURSEMENTS.to_vec(),
            Self::Testnet(params) if params.is_default_testnet() => {
                subsidy::constants::testnet::NU6_1_LOCKBOX_DISBURSEMENTS.to_vec()
            }
            Self::Testnet(params) => return params.lockbox_disbursements(),
        };

        expected_lockbox_disbursements
            .into_iter()
            .map(|(addr, amount)| {
                (
                    addr.parse().expect("hard-coded address must deserialize"),
                    amount,
                )
            })
            .collect()
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

impl zcash_protocol::consensus::Parameters for Network {
    fn network_type(&self) -> zcash_protocol::consensus::NetworkType {
        self.kind().into()
    }

    fn activation_height(
        &self,
        nu: zcash_protocol::consensus::NetworkUpgrade,
    ) -> Option<zcash_protocol::consensus::BlockHeight> {
        NetworkUpgrade::from(nu)
            .activation_height(self)
            .map(|Height(h)| zcash_protocol::consensus::BlockHeight::from_u32(h))
    }
}
