//! Sprout Shielded Payment Address types.

use std::{fmt, io};

use bs58;
use ripemd160::{Digest, Ripemd160};
use sha2::Sha256;
use x25519_dalek;

#[cfg(test)]
use proptest::{arbitrary::Arbitrary, collection::vec, prelude::*};

use crate::{
    keys::sprout,
    serialization::{
        ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
    },
    Network,
};

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
#[derive(Clone, Eq, PartialEq)]
pub struct SproutShieldedAddress {
    network: Network,
    paying_key: sprout::PayingKey,
    transmission_key: x25519_dalek::PublicKey,
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

impl ZcashSerialize for SproutShieldedAddress {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        if self.network == Network::Mainnet {
            writer.write_all(&[0x16, 0x9A][..])?
        } else {
            writer.write_all(&[0x16, 0xB6][..])?
        }
        writer.write_all(&self.paying_key.0[..])?;
        // XXX revisit to see if we want to impl ZcashSerialize on
        // x25519_dalek::PublicKey
        writer.write_all(self.transmission_key.as_bytes())?;

        Ok(())
    }
}

impl ZcashDeserialize for SproutShieldedAddress {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut version_bytes = [0; 2];
        reader.read_exact(&mut version_bytes)?;

        let network = match version_bytes {
            [0x16, 0x9A] => Network::Mainnet,
            [0x16, 0xB6] => Network::Testnet,
            _ => panic!(SerializationError::Parse(
                "bad sprout shielded addr version/type",
            )),
        };

        Ok(SproutShieldedAddress {
            network,
            paying_key: sprout::PayingKey(reader.read_32_bytes()?),
            transmission_key: sprout::TransmissionKey(reader.read_32_bytes()?),
        })
    }
}
