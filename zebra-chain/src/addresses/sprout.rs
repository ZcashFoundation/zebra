//! Sprout Shielded Payment Address types.

use std::{fmt, io};

use bs58;

#[cfg(test)]
use proptest::{arbitrary::Arbitrary, array, prelude::*};

use crate::{
    keys::sprout,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
    Network,
};

/// Magic numbers used to identify what networks Sprout Shielded
/// Addresses are associated with.
mod magics {
    pub const MAINNET: [u8; 2] = [0x16, 0x9A];
    pub const TESTNET: [u8; 2] = [0x16, 0xB6];
}

/// Sprout Shielded Payment Addresses
///
/// In Bitcoin a single byte is used for the version field identifying
/// the address type. In Zcash two bytes are used. For addresses on
/// the production network, this and the encoded length cause the first
/// two characters of the Base58Check encoding to be xed as “t3” for
/// P2SH addresses, and as “t1” for P2PKH addresses. (This does not
/// imply that a transparent Zcash address can be parsed identically
/// to a Bitcoin address just by removing the “t”.)
///
/// https://zips.z.cash/protocol/protocol.pdf#transparentaddrencoding
#[derive(Copy, Clone)]
pub struct SproutShieldedAddress {
    network: Network,
    paying_key: sprout::PayingKey,
    transmission_key: sprout::TransmissionKey,
}

impl fmt::Debug for SproutShieldedAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut bytes = io::Cursor::new(Vec::new());
        let _ = self.zcash_serialize(&mut bytes);

        f.debug_tuple("SproutShieldedAddress")
            .field(&bs58::encode(bytes.get_ref()).with_check().into_string())
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
        if self.network == Network::Mainnet {
            writer.write_all(&magics::MAINNET[..])?
        } else {
            writer.write_all(&magics::TESTNET[..])?
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
            _ => panic!(SerializationError::Parse(
                "bad sprout shielded addr version/type",
            )),
        };

        Ok(SproutShieldedAddress {
            network,
            paying_key: sprout::PayingKey(reader.read_32_bytes()?),
            transmission_key: sprout::TransmissionKey::from(reader.read_32_bytes()?),
        })
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
            .prop_map(|(network, paying_key_bytes, transmission_key_bytes)| {
                return Self {
                    network,
                    paying_key: sprout::PayingKey(paying_key_bytes),
                    transmission_key: sprout::TransmissionKey::from(transmission_key_bytes),
                };
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
proptest! {

    #[test]
    fn sprout_address_roundtrip(zaddr in any::<SproutShieldedAddress>()) {

        let mut data = Vec::new();

        zaddr.zcash_serialize(&mut data).expect("sprout z-addr should serialize");

        let zaddr2 = SproutShieldedAddress::zcash_deserialize(&data[..])
            .expect("randomized sprout z-addr should deserialize");

        prop_assert_eq![zaddr, zaddr2];
    }
}
