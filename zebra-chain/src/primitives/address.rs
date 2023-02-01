//! `zcash_address` conversion to `zebra_chain` address types.
//!
//! Usage: <https://docs.rs/zcash_address/0.2.0/zcash_address/trait.TryFromAddress.html#examples>

use zcash_primitives::sapling;

use crate::{orchard, parameters::Network, transparent, BoxError};

/// Zcash address variants
// TODO: Add Sprout addresses
pub enum Address {
    /// Transparent address
    Transparent(transparent::Address),

    /// Sapling address
    Sapling {
        /// Address' network
        network: Network,

        /// Sapling address
        address: sapling::PaymentAddress,
    },

    /// Unified address
    Unified {
        /// Address' network
        network: Network,

        /// Transparent address
        transparent_address: transparent::Address,

        /// Sapling address
        sapling_address: sapling::PaymentAddress,

        /// Orchard address
        orchard_address: orchard::Address,
    },
}

impl TryFrom<zcash_address::Network> for Network {
    // TODO: better error type
    type Error = BoxError;

    fn try_from(network: zcash_address::Network) -> Result<Self, Self::Error> {
        match network {
            zcash_address::Network::Main => Ok(Network::Mainnet),
            zcash_address::Network::Test => Ok(Network::Testnet),
            zcash_address::Network::Regtest => Err("unsupported Zcash network parameters".into()),
        }
    }
}

impl From<Network> for zcash_address::Network {
    fn from(network: Network) -> Self {
        match network {
            Network::Mainnet => zcash_address::Network::Main,
            Network::Testnet => zcash_address::Network::Test,
        }
    }
}

impl zcash_address::TryFromAddress for Address {
    // TODO: crate::serialization::SerializationError
    type Error = BoxError;

    fn try_from_transparent_p2pkh(
        network: zcash_address::Network,
        data: [u8; 20],
    ) -> Result<Self, zcash_address::ConversionError<Self::Error>> {
        Ok(Self::Transparent(transparent::Address::from_pub_key_hash(
            network.try_into()?,
            data,
        )))
    }

    fn try_from_transparent_p2sh(
        network: zcash_address::Network,
        data: [u8; 20],
    ) -> Result<Self, zcash_address::ConversionError<Self::Error>> {
        Ok(Self::Transparent(transparent::Address::from_script_hash(
            network.try_into()?,
            data,
        )))
    }

    fn try_from_sapling(
        network: zcash_address::Network,
        data: [u8; 43],
    ) -> Result<Self, zcash_address::ConversionError<Self::Error>> {
        let network = network.try_into()?;
        sapling::PaymentAddress::from_bytes(&data)
            .map(|address| Self::Sapling { address, network })
            .ok_or_else(|| BoxError::from("not a valid sapling address").into())
    }

    // TODO: Add sprout and unified/orchard converters
}

impl Address {
    /// Returns the network for the address.
    pub fn network(&self) -> Network {
        match &self {
            Self::Transparent(address) => address.network(),
            Self::Sapling { network, .. } | Self::Unified { network, .. } => *network,
        }
    }

    /// Returns true if the address is PayToScriptHash
    /// Returns false if the address is PayToPublicKeyHash or shielded.
    pub fn is_script_hash(&self) -> bool {
        match &self {
            Self::Transparent(address) => address.is_script_hash(),
            Self::Sapling { .. } | Self::Unified { .. } => false,
        }
    }

    /// Returns true if address is of the [`Address::Transparent`] variant.
    /// Returns false if otherwise.
    pub fn is_transparent(&self) -> bool {
        matches!(self, Self::Transparent(_))
    }
}
