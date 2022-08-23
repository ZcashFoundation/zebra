//! Sprout Shielded Payment Address types.

use std::{fmt, io};

#[cfg(test)]
use proptest::{arbitrary::Arbitrary, array, prelude::*};

use crate::{
    parameters::Network,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
};

use super::keys;

/// Magic numbers used to identify what networks Sprout Shielded
/// Addresses are associated with.
mod magics {
    pub const MAINNET: [u8; 2] = [0x16, 0x9A];
    pub const TESTNET: [u8; 2] = [0x16, 0xB6];
}

/// Sprout Shielded Payment Addresses
///
/// <https://zips.z.cash/protocol/protocol.pdf#sproutpaymentaddrencoding>
#[derive(Copy, Clone)]
pub struct SproutShieldedAddress {
    network: Network,
    paying_key: keys::PayingKey,
    transmission_key: keys::TransmissionKey,
}

impl fmt::Debug for SproutShieldedAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("SproutShieldedAddress")
            .field("network", &self.network)
            .field("paying_key", &self.paying_key)
            // Use hex formatting for the transmission key.
            .field(
                "transmission_key",
                &hex::encode(&self.transmission_key.as_bytes()),
            )
            .finish()
    }
}

impl PartialEq for SproutShieldedAddress {
    fn eq(&self, other: &Self) -> bool {
        self.network == other.network
            && self.paying_key.0 == other.paying_key.0
            && self.transmission_key.as_bytes() == other.transmission_key.as_bytes()
    }
}

impl Eq for SproutShieldedAddress {}

impl ZcashSerialize for SproutShieldedAddress {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self.network {
            Network::Mainnet => writer.write_all(&magics::MAINNET[..])?,
            _ => writer.write_all(&magics::TESTNET[..])?,
        }
        writer.write_all(&self.paying_key.0[..])?;
        writer.write_all(self.transmission_key.as_bytes())?;

        Ok(())
    }
}

impl ZcashDeserialize for SproutShieldedAddress {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut version_bytes = [0; 2];
        reader.read_exact(&mut version_bytes)?;

        let network = match version_bytes {
            magics::MAINNET => Network::Mainnet,
            magics::TESTNET => Network::Testnet,
            _ => panic!("SerializationError: bad sprout shielded addr version/type"),
        };

        Ok(SproutShieldedAddress {
            network,
            paying_key: keys::PayingKey(reader.read_32_bytes()?),
            transmission_key: keys::TransmissionKey::from(reader.read_32_bytes()?),
        })
    }
}

impl fmt::Display for SproutShieldedAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut bytes = io::Cursor::new(Vec::new());

        let _ = self.zcash_serialize(&mut bytes);

        f.write_str(&bs58::encode(bytes.get_ref()).with_check().into_string())
    }
}

impl std::str::FromStr for SproutShieldedAddress {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let result = &bs58::decode(s).with_check(None).into_vec();

        match result {
            Ok(bytes) => Self::zcash_deserialize(&bytes[..]),
            Err(_) => Err(SerializationError::Parse("bs58 decoding error")),
        }
    }
}

#[cfg(test)]
impl Arbitrary for SproutShieldedAddress {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<Network>(),
            array::uniform32(any::<u8>()),
            array::uniform32(any::<u8>()),
        )
            .prop_map(|(network, paying_key_bytes, transmission_key_bytes)| Self {
                network,
                paying_key: keys::PayingKey(paying_key_bytes),
                transmission_key: keys::TransmissionKey::from(transmission_key_bytes),
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn from_string_debug() {
        let _init_guard = zebra_test::init();

        let string = "zcU1Cd6zYyZCd2VJF8yKgmzjxdiiU1rgTTjEwoN1CGUWCziPkUTXUjXmX7TMqdMNsTfuiGN1jQoVN4kGxUR4sAPN4XZ7pxb";
        let zc_addr = string.parse::<SproutShieldedAddress>().unwrap();

        assert_eq!(string, zc_addr.to_string());
    }
}

#[cfg(test)]
proptest! {

    #[test]
    fn zcash_de_serialize_roundtrip(zaddr in any::<SproutShieldedAddress>()) {
        let _init_guard = zebra_test::init();

        let mut data = Vec::new();

        zaddr.zcash_serialize(&mut data).expect("sprout z-addr should serialize");

        let zaddr2 = SproutShieldedAddress::zcash_deserialize(&data[..])
            .expect("randomized sprout z-addr should deserialize");

        prop_assert_eq![zaddr, zaddr2];
    }

    #[test]
    fn zcash_base58check_roundtrip(zaddr in any::<SproutShieldedAddress>()) {
        let _init_guard = zebra_test::init();

        let string = zaddr.to_string();

        let zaddr2 = string.parse::<SproutShieldedAddress>().unwrap();

        prop_assert_eq![zaddr, zaddr2];
    }
}
