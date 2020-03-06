//! Address types.

use std::{fmt, io};

use bs58;
use ripemd160::{Digest, Ripemd160};
use secp256k1::PublicKey;
use sha2::Sha256;

#[cfg(test)]
use proptest_derive::Arbitrary;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

use crate::types::Script;

/// A hash of a redeem script, as used in transparent
/// pay-to-script-hash and pay-to-publickey-hash addresses.
///
/// The resulting hash in both of these cases is always exactly 20
/// bytes. These are big-endian.
/// https://en.bitcoin.it/Base58Check_encoding#Encoding_a_Bitcoin_address
#[derive(Copy, Clone, Eq, PartialEq)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct AddressPayloadHash(pub [u8; 20]);

impl<'a> From<&'a [u8]> for AddressPayloadHash {
    fn from(bytes: &'a [u8]) -> Self {
        let sha_hash = Sha256::digest(bytes);
        let ripe_hash = Ripemd160::digest(&sha_hash);
        let mut payload = [0u8; 20];
        payload[..].copy_from_slice(&ripe_hash[..]);
        Self(payload)
    }
}

impl fmt::Debug for AddressPayloadHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("AddressPayloadHash")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

/// An enum describing the possible network choices.
// XXX Stolen from zebra-network for now.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Network {
    /// The production mainnet.
    Mainnet,
    /// The testnet.
    Testnet,
}

/// Transparent Zcash Addresses
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
#[derive(Copy, Clone, Eq, PartialEq)]
// #[cfg_attr(test, derive(Arbitrary))]
pub enum TransparentAddress {
    /// P2SH (Pay to Script Hash) addresses
    PayToScriptHash {
        /// Production, test, or other network
        network: Network,
        /// 20 bytes specifying a script hash.
        script_hash: AddressPayloadHash,
    },
    /// P2PKH (Pay to Public Key Hash) addresses
    PayToPublicKeyHash {
        /// Production, test, or other network
        network: Network,
        /// 20 bytes specifying a public key hash, which is a RIPEMD-160
        /// hash of a SHA-256 hash of a compressed ECDSA key encoding.
        pub_key_hash: AddressPayloadHash,
    },
}

impl From<Script> for TransparentAddress {
    fn from(script: Script) -> Self {
        TransparentAddress::PayToScriptHash {
            network: Network::Mainnet,
            script_hash: AddressPayloadHash::from(&script.0[..]),
        }
    }
}

impl From<PublicKey> for TransparentAddress {
    fn from(pub_key: PublicKey) -> Self {
        TransparentAddress::PayToPublicKeyHash {
            network: Network::Mainnet,
            pub_key_hash: AddressPayloadHash::from(&pub_key.serialize()[..]),
        }
    }
}

impl fmt::Debug for TransparentAddress {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut bytes = io::Cursor::new(Vec::new());
        let _ = self.zcash_serialize(&mut bytes);

        f.debug_tuple("TransparentAddress")
            .field(&bs58::encode(bytes.get_ref()).into_vec())
            .finish()
    }
}

impl ZcashSerialize for TransparentAddress {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self {
            TransparentAddress::PayToScriptHash {
                network,
                script_hash,
            } => {
                if *network == Network::Mainnet {
                    writer.write_all(&[0x1C, 0xBD][..])?
                } else {
                    // Dev network doesn't have a recommendation so we
                    // default to testnet bytes.
                    writer.write_all(&[0x1C, 0xBA][..])?
                }
                // XXX I'm asuming this is BigEndian because we're not
                // explicitly making it LittleEndian?
                writer.write_all(&script_hash.0)?
            }
            TransparentAddress::PayToPublicKeyHash {
                network,
                pub_key_hash,
            } => {
                if *network == Network::Mainnet {
                    writer.write_all(&[0x1C, 0xB8][..])?
                } else {
                    // Dev network doesn't have a recommendation so we
                    // default to testnet bytes.
                    writer.write_all(&[0x1D, 0x25][..])?
                }
                // XXX I'm asuming this is BigEndian because we're not
                // explicitly making it LittleEndian?
                writer.write_all(&pub_key_hash.0)?
            }
        }
        Ok(())
    }
}

impl ZcashDeserialize for TransparentAddress {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut version_bytes = [0; 2];
        reader.read_exact(&mut version_bytes)?;

        let mut hash_bytes = [0; 20];
        reader.read_exact(&mut hash_bytes)?;

        match version_bytes {
            [0x1c, 0xbd] => Ok(TransparentAddress::PayToScriptHash {
                network: Network::Mainnet,
                script_hash: AddressPayloadHash(hash_bytes),
            }),
            // Currently only reading !mainnet versions as testnet.
            [0x1c, 0xba] => Ok(TransparentAddress::PayToScriptHash {
                network: Network::Testnet,
                script_hash: AddressPayloadHash(hash_bytes),
            }),
            [0x1c, 0xb8] => Ok(TransparentAddress::PayToPublicKeyHash {
                network: Network::Mainnet,
                pub_key_hash: AddressPayloadHash(hash_bytes),
            }),
            // Currently only reading !mainnet versions as testnet.
            [0x1d, 0x25] => Ok(TransparentAddress::PayToPublicKeyHash {
                network: Network::Testnet,
                pub_key_hash: AddressPayloadHash(hash_bytes),
            }),
            _ => Err(SerializationError::Parse("bad t-addr version/type")),
        }
    }
}
