//! `zcash_address` conversion to `zebra_chain` address types.
//!
//! Usage: <https://docs.rs/zcash_address/0.2.0/zcash_address/trait.TryFromAddress.html#examples>

use zcash_address::unified::{self, Container};
use zcash_primitives::sapling;

use crate::{
    parameters::{Network, NetworkKind, UnsupportedNetwork},
    transparent, BoxError,
};

/// Zcash address variants
pub enum Address {
    /// Transparent address
    Transparent(transparent::Address),

    /// Sapling address
    Sapling {
        /// Address' network kind
        network: NetworkKind,

        /// Sapling address
        address: sapling::PaymentAddress,
    },

    /// Unified address
    Unified {
        /// Address' network kind
        network: NetworkKind,

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
            zcash_address::Network::Test => Ok(Network::new_default_testnet()),
            // TODO: Add conversion to regtest (#7839)
            zcash_address::Network::Regtest => Err("unsupported Zcash network parameters".into()),
        }
    }
}

impl TryFrom<&Network> for zcash_address::Network {
    type Error = UnsupportedNetwork;

    fn try_from(network: &Network) -> Result<Self, Self::Error> {
        match network {
            Network::Mainnet => Ok(zcash_address::Network::Main),
            Network::Testnet(_params) if network.is_default_testnet() => {
                Ok(zcash_address::Network::Test)
            }
            // TODO: If the network parameters match `Regtest` parameters, convert to
            //       `zcash_address::Network::Regtest instead of returning `UnsupportedAddress` error.
            //       (#7119, #7839)
            Network::Testnet(_params) => Err(UnsupportedNetwork(
                "could not convert configured testnet to zcash_address::Network".to_string(),
            )),
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
            NetworkKind::from_zcash_address(network),
            data,
        )))
    }

    fn try_from_transparent_p2sh(
        network: zcash_address::Network,
        data: [u8; 20],
    ) -> Result<Self, zcash_address::ConversionError<Self::Error>> {
        Ok(Self::Transparent(transparent::Address::from_script_hash(
            NetworkKind::from_zcash_address(network),
            data,
        )))
    }

    fn try_from_sapling(
        network: zcash_address::Network,
        data: [u8; 43],
    ) -> Result<Self, zcash_address::ConversionError<Self::Error>> {
        let network = NetworkKind::from_zcash_address(network);
        sapling::PaymentAddress::from_bytes(&data)
            .map(|address| Self::Sapling { address, network })
            .ok_or_else(|| BoxError::from("not a valid sapling address").into())
    }

    fn try_from_unified(
        network: zcash_address::Network,
        unified_address: zcash_address::unified::Address,
    ) -> Result<Self, zcash_address::ConversionError<Self::Error>> {
        let network = NetworkKind::from_zcash_address(network);
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
    pub fn network(&self) -> NetworkKind {
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
                let address = ZcashAddress::from_sapling(network.to_zcash_address(), data);
                Some(address.encode())
            }
            Self::Unified { .. } => None,
        }
    }
}

impl NetworkKind {
    /// Converts a [`zcash_address::Network`] to a [`NetworkKind`].
    ///
    /// This method is meant to be used for decoding Zcash addresses in zebra-rpc methods.
    fn from_zcash_address(network: zcash_address::Network) -> Self {
        match network {
            zcash_address::Network::Main => NetworkKind::Mainnet,
            zcash_address::Network::Test => NetworkKind::Testnet,
            zcash_address::Network::Regtest => NetworkKind::Regtest,
        }
    }

    /// Converts a [`zcash_address::Network`] to a [`NetworkKind`].
    ///
    /// This method is meant to be used for encoding Zcash addresses in zebra-rpc methods.
    fn to_zcash_address(self) -> zcash_address::Network {
        match self {
            NetworkKind::Mainnet => zcash_address::Network::Main,
            NetworkKind::Testnet => zcash_address::Network::Test,
            NetworkKind::Regtest => zcash_address::Network::Regtest,
        }
    }
}
