//! Consensus parameters for each Zcash network.

use std::{fmt, str::FromStr};

use thiserror::Error;

use crate::{
    block::{Height, HeightDiff},
    parameters::NetworkUpgrade::Canopy,
};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

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
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub enum Network {
    /// The production mainnet.
    #[default]
    Mainnet,

    /// The oldest public test network.
    Testnet,
}

/// A magic number identifying the network.
#[derive(Copy, Clone, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Magic(pub [u8; 4]);

impl fmt::Debug for Magic {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Magic").field(&hex::encode(self.0)).finish()
    }
}

/// Magic numbers used to identify different Zcash networks.
pub mod magics {
    use super::*;
    /// The production mainnet.
    pub const MAINNET: Magic = Magic([0x24, 0xe9, 0x27, 0x64]);
    /// The testnet.
    pub const TESTNET: Magic = Magic([0xfa, 0x1a, 0xf9, 0xbf]);
}
pub trait AllParameters: zcash_primitives::consensus::Parameters {
    fn height_for_first_halving(&self) -> Height;
    fn genesis_hash(&self) -> crate::block::Hash;
    fn magic_value(&self) -> Magic;
}
impl AllParameters for Network {
    fn height_for_first_halving(&self) -> Height {
        match self {
            Network::Mainnet => Canopy
                .activation_height(*self)
                .expect("canopy activation height should be available"),
            Network::Testnet => constants::FIRST_HALVING_TESTNET,
        }
    }

    fn genesis_hash(&self) -> crate::block::Hash {
        match self {
            // zcash-cli getblockhash 0
            Network::Mainnet => "00040fe8ec8471911baa1db1266ea15dd06b4a8a5c453883c000b031973dce08",
            // zcash-cli -testnet getblockhash 0
            Network::Testnet => "05a60a92d99d85997cce3b87616c089f6124d7342af37106edc76126334a2c38",
        }
        .parse()
        .expect("hard-coded hash parses")
    }

    fn magic_value(&self) -> Magic {
        match self {
            Network::Mainnet => magics::MAINNET,
            Network::Testnet => magics::TESTNET,
        }
    }
}
impl zcash_primitives::consensus::Parameters for Network {
    fn activation_height(
        &self,
        nu: zcash_primitives::consensus::NetworkUpgrade,
    ) -> Option<zcash_primitives::consensus::BlockHeight> {
        todo!()
    }

    fn coin_type(&self) -> u32 {
        match self {
            Network::Mainnet => zcash_primitives::constants::mainnet::COIN_TYPE,
            Network::Testnet => zcash_primitives::constants::testnet::COIN_TYPE,
        }
    }

    fn address_network(&self) -> Option<zcash_address::Network> {
        todo!()
    }

    fn hrp_sapling_extended_spending_key(&self) -> &str {
        todo!()
    }

    fn hrp_sapling_extended_full_viewing_key(&self) -> &str {
        todo!()
    }

    fn hrp_sapling_payment_address(&self) -> &str {
        todo!()
    }

    fn b58_pubkey_address_prefix(&self) -> [u8; 2] {
        match self {
            Network::Mainnet => zcash_primitives::constants::mainnet::B58_PUBKEY_ADDRESS_PREFIX,
            Network::Testnet => zcash_primitives::constants::testnet::B58_PUBKEY_ADDRESS_PREFIX,
        }
    }

    fn b58_script_address_prefix(&self) -> [u8; 2] {
        todo!()
    }
}
impl From<Network> for &'static str {
    fn from(network: Network) -> &'static str {
        match network {
            Network::Mainnet => "Mainnet",
            Network::Testnet => "Testnet",
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
    /// Returns an iterator over [`Network`] variants.
    pub fn iter() -> impl Iterator<Item = Self> {
        // TODO: Use default values of `Testnet` variant when adding fields for #7845.
        [Self::Mainnet, Self::Testnet].into_iter()
    }

    /// Get the default port associated to this network.
    pub fn default_port(&self) -> u16 {
        match self {
            Network::Mainnet => 8233,
            Network::Testnet => 18233,
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
            Network::Testnet => "test".to_string(),
        }
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
    pub fn sapling_activation_height(self) -> Height {
        super::NetworkUpgrade::Sapling
            .activation_height(self)
            .expect("Sapling activation height needs to be set")
    }
}

impl FromStr for Network {
    type Err = InvalidNetworkError;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        match string.to_lowercase().as_str() {
            "mainnet" => Ok(Network::Mainnet),
            "testnet" => Ok(Network::Testnet),
            _ => Err(InvalidNetworkError(string.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Error)]
#[error("Invalid network: {0}")]
pub struct InvalidNetworkError(String);
