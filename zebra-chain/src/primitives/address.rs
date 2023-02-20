//! `zcash_address` conversion to `zebra_chain` address types.
//!
//! Usage: <https://docs.rs/zcash_address/0.2.0/zcash_address/trait.TryFromAddress.html#examples>

use zcash_address::unified::{self, Container};
use zcash_primitives::sapling;

use crate::{parameters::Network, transparent, BoxError};

/// Zcash address variants
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

        /// Unified address
        unified_address: zcash_address::unified::Address,

        /// Orchard address
        orchard: Option<orchard::Address>,

        /// Sapling address
        sapling: Option<sapling::PaymentAddress>,

        /// Transparent address
        transparent: Option<transparent::Address>,
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

    fn try_from_unified(
        network: zcash_address::Network,
        unified_address: zcash_address::unified::Address,
    ) -> Result<Self, zcash_address::ConversionError<Self::Error>> {
        let network = network.try_into()?;
        let mut orchard = None;
        let mut sapling = None;
        let mut transparent = None;

        for receiver in unified_address.items().into_iter() {
            match receiver {
                unified::Receiver::Orchard(data) => {
                    orchard = orchard::Address::from_raw_address_bytes(&data).into();
                    // ZIP 316: Consumers MUST reject Unified Addresses/Viewing Keys in
                    // which any constituent Item does not meet the validation
                    // requirements of its encoding.
                    if orchard.is_none() {
                        return Err(BoxError::from(
                            "Unified Address contains an invalid Orchard receiver.",
                        )
                        .into());
                    }
                }
                unified::Receiver::Sapling(data) => {
                    sapling = sapling::PaymentAddress::from_bytes(&data);
                    // ZIP 316: Consumers MUST reject Unified Addresses/Viewing Keys in
                    // which any constituent Item does not meet the validation
                    // requirements of its encoding.
                    if sapling.is_none() {
                        return Err(BoxError::from(
                            "Unified Address contains an invalid Sapling receiver",
                        )
                        .into());
                    }
                }
                unified::Receiver::P2pkh(data) => {
                    transparent = Some(transparent::Address::from_pub_key_hash(network, data));
                }
                unified::Receiver::P2sh(data) => {
                    transparent = Some(transparent::Address::from_script_hash(network, data));
                }
                unified::Receiver::Unknown { .. } => {
                    return Err(BoxError::from("Unsupported receiver in a Unified Address.").into());
                }
            }
        }

        Ok(Self::Unified {
            network,
            unified_address,
            orchard,
            sapling,
            transparent,
        })
    }
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

    /// Returns the payment address for transparent or sapling addresses.
    pub fn payment_address(&self) -> Option<String> {
        use zcash_address::{ToAddress, ZcashAddress};

        match &self {
            Self::Transparent(address) => Some(address.to_string()),
            Self::Sapling { address, network } => {
                let data = address.to_bytes();
                let address = ZcashAddress::from_sapling((*network).into(), data);
                Some(address.encode())
            }
            Self::Unified { .. } => None,
        }
    }
}
