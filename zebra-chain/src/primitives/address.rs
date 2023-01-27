//! `zcash_address` conversion to `zebra_chain` address types.
//!
//! Usage: <https://docs.rs/zcash_address/0.2.0/zcash_address/trait.TryFromAddress.html#examples>

use crate::{parameters::Network, transparent, BoxError};

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

// TODO: implement sprout, sapling, and unified on those types
impl zcash_address::TryFromAddress for transparent::Address {
    // TODO: crate::serialization::SerializationError
    type Error = BoxError;

    fn try_from_transparent_p2pkh(
        network: zcash_address::Network,
        data: [u8; 20],
    ) -> Result<Self, zcash_address::ConversionError<Self::Error>> {
        let network = network.try_into()?;

        Ok(transparent::Address::from_pub_key_hash(network, data))
    }

    fn try_from_transparent_p2sh(
        network: zcash_address::Network,
        data: [u8; 20],
    ) -> Result<Self, zcash_address::ConversionError<Self::Error>> {
        let network = network.try_into()?;

        Ok(transparent::Address::from_script_hash(network, data))
    }
}
