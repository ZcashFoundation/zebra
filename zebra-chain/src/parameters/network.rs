//! Consensus parameters for each Zcash network.

use std::{fmt, str::FromStr};

use thiserror::Error;

use crate::{
    block::{Height, HeightDiff},
    parameters::NetworkUpgrade::{self, Canopy},
};

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

/// An enum describing the possible network choices.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Network {
    /// The production mainnet.
    #[default]
    Mainnet,

    /// The oldest public test network.
    Testnet(#[serde(skip)] TestnetParameters),
}

/// Configures testnet network parameters to use instead of default values.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Serialize)]
pub struct TestnetParameters(Option<NetworkParameters>);

impl std::ops::Deref for TestnetParameters {
    type Target = Option<NetworkParameters>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Option<NetworkParameters>> for TestnetParameters {
    fn from(value: Option<NetworkParameters>) -> Self {
        Self(value)
    }
}

impl TestnetParameters {
    /// Returns `network_id` in network parameters, if any.
    pub fn network_id(self) -> Option<[u8; 4]> {
        self.0?.network_id()
    }

    /// Returns `default_port` in network parameters, if any.
    pub fn default_port(self) -> Option<u16> {
        self.0?.default_port()
    }

    /// Returns `cache_name` in network parameters, if any.
    pub fn cache_name(self) -> Option<&'static str> {
        self.0?.cache_name()
    }

    /// Returns `genesis_hash` in network parameters, if any.
    pub fn genesis_hash(self) -> Option<&'static str> {
        self.0?.genesis_hash()
    }

    /// Returns `activation_heights` in network parameters, if any.
    pub fn activation_heights(self) -> Option<&'static [(Height, NetworkUpgrade)]> {
        self.0?.activation_heights()
    }
}

/// Configures network parameters to use instead of default values.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Serialize)]
pub struct NetworkParameters {
    /// Used as the network magic, Zebra will reject messages and connections from peers
    /// with a different network magic.
    network_id: Option<[u8; 4]>,

    /// Default port for the network
    default_port: Option<u16>,

    /// Network portion of zebra-state cache path
    cache_name: Option<&'static str>,

    /// Genesis hash for this testnet
    genesis_hash: Option<&'static str>,

    /// Activation heights for this testnet
    activation_heights: Option<&'static [(Height, NetworkUpgrade)]>,
}

impl NetworkParameters {
    fn network_id(self) -> Option<[u8; 4]> {
        self.network_id
    }
    fn default_port(self) -> Option<u16> {
        self.default_port
    }
    fn cache_name(self) -> Option<&'static str> {
        self.cache_name
    }
    fn genesis_hash(self) -> Option<&'static str> {
        self.genesis_hash
    }
    fn activation_heights(self) -> Option<&'static [(Height, NetworkUpgrade)]> {
        self.activation_heights
    }
}

/// The network id to expect in AddrV2 messages.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct NetworkId {
    ipv4: u8,
    ipv6: u8,
}
impl From<Network> for &'static str {
    fn from(network: Network) -> &'static str {
        match network {
            Network::Mainnet => "Mainnet",
            Network::Testnet(_) => "Testnet",
        }
    }
}

impl From<&Network> for &'static str {
    fn from(network: &Network) -> &'static str {
        (*network).into()
    }
}

impl fmt::Display for Network {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.into())
    }
}

impl Network {
    /// Creates a new `Testnet` with no parameters.
    pub fn new_testnet() -> Self {
        Self::new_testnet_with_parameters(None)
    }

    /// Creates a new `Testnet` with the provided parameters.
    pub fn new_testnet_with_parameters(parameters: impl Into<TestnetParameters>) -> Self {
        Self::Testnet(parameters.into())
    }

    /// Creates a new `Mainnet`.
    pub fn new_mainnet() -> Self {
        Self::Mainnet
    }

    /// Get the default port associated to this network.
    pub fn default_port(&self) -> u16 {
        match self {
            Network::Mainnet => 8233,
            Network::Testnet(params) => params.default_port().unwrap_or(18233),
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

        let canopy_activation = Canopy
            .activation_height(*self)
            .expect("Canopy activation height must be present for both networks");

        (canopy_activation + ZIP_212_GRACE_PERIOD_DURATION)
            .expect("ZIP-212 grace period ends at a valid block height")
    }

    /// Return the network name as defined in
    /// [BIP70](https://github.com/bitcoin/bips/blob/master/bip-0070.mediawiki#paymentdetailspaymentrequest)
    pub fn bip70_network_name(&self) -> String {
        match self {
            Network::Mainnet => "main".to_string(),
            // TODO: add network name field?
            Network::Testnet { .. } => "test".to_string(),
        }
    }

    /// Return the lowercase network name.
    pub fn lowercase_name(&self) -> String {
        self.to_string().to_ascii_lowercase()
    }

    /// Return the network cache name.
    pub fn cache_name(&self) -> String {
        self.testnet_parameters()
            .cache_name()
            .map_or_else(|| self.lowercase_name(), ToString::to_string)
    }

    /// Returns `true` if this network is `Mainnet`.
    pub fn is_mainnet(&self) -> bool {
        *self == Network::Mainnet
    }

    /// Returns `true` if this network is a testing network.
    pub fn is_a_test_network(&self) -> bool {
        matches!(*self, Network::Testnet(_))
    }

    /// Returns testnet parameters, if any.
    pub fn testnet_parameters(&self) -> TestnetParameters {
        if let Network::Testnet(testnet_parameters) = *self {
            testnet_parameters
        } else {
            None.into()
        }
    }
}

impl FromStr for Network {
    type Err = InvalidNetworkError;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        match string.to_lowercase().as_str() {
            "mainnet" => Ok(Network::Mainnet),
            "testnet" => Ok(Network::new_testnet()),
            _ => Err(InvalidNetworkError(string.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Error)]
#[error("Invalid network: {0}")]
pub struct InvalidNetworkError(String);
