//! Transparent Address types.

use std::{fmt, io};

use ripemd::{Digest, Ripemd160};
use secp256k1::PublicKey;
use sha2::Sha256;

use crate::{
    parameters::Network,
    serialization::{SerializationError, ZcashDeserialize, ZcashSerialize},
    transparent::{opcodes::OpCode, Script},
};

#[cfg(test)]
use proptest::prelude::*;

/// Transparent Zcash Addresses
///
/// In Bitcoin a single byte is used for the version field identifying
/// the address type. In Zcash two bytes are used. For addresses on
/// the production network, this and the encoded length cause the first
/// two characters of the Base58Check encoding to be fixed as "t3" for
/// P2SH addresses, and as "t1" for P2PKH addresses. (This does not
/// imply that a transparent Zcash address can be parsed identically
/// to a Bitcoin address just by removing the "t".)
///
/// <https://zips.z.cash/protocol/protocol.pdf#transparentaddrencoding>
#[derive(
    Copy, Clone, Eq, PartialEq, Hash, serde_with::SerializeDisplay, serde_with::DeserializeFromStr,
)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub enum Address {
    /// P2SH (Pay to Script Hash) addresses
    PayToScriptHash {
        /// Production, test, or other network
        network: Network,
        /// 20 bytes specifying a script hash.
        script_hash: [u8; 20],
    },

    /// P2PKH (Pay to Public Key Hash) addresses
    PayToPublicKeyHash {
        /// Production, test, or other network
        network: Network,
        /// 20 bytes specifying a public key hash, which is a RIPEMD-160
        /// hash of a SHA-256 hash of a compressed ECDSA key encoding.
        pub_key_hash: [u8; 20],
    },
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut debug_struct = f.debug_struct("TransparentAddress");

        match self {
            Address::PayToScriptHash {
                network,
                script_hash,
            } => debug_struct
                .field("network", network)
                .field("script_hash", &hex::encode(script_hash))
                .finish(),
            Address::PayToPublicKeyHash {
                network,
                pub_key_hash,
            } => debug_struct
                .field("network", network)
                .field("pub_key_hash", &hex::encode(pub_key_hash))
                .finish(),
        }
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut bytes = io::Cursor::new(Vec::new());
        let _ = self.zcash_serialize(&mut bytes);

        f.write_str(&bs58::encode(bytes.get_ref()).with_check().into_string())
    }
}

impl std::str::FromStr for Address {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let result = &bs58::decode(s).with_check(None).into_vec();

        match result {
            Ok(bytes) => Self::zcash_deserialize(&bytes[..]),
            Err(_) => Err(SerializationError::Parse("t-addr decoding error")),
        }
    }
}

impl ZcashSerialize for Address {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self {
            Address::PayToScriptHash {
                network,
                script_hash,
            } => {
                writer.write_all(&network.b58_script_address_prefix())?;
                writer.write_all(script_hash)?
            }
            Address::PayToPublicKeyHash {
                network,
                pub_key_hash,
            } => {
                writer.write_all(&network.b58_pubkey_address_prefix())?;
                writer.write_all(pub_key_hash)?
            }
        }

        Ok(())
    }
}

impl ZcashDeserialize for Address {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut version_bytes = [0; 2];
        reader.read_exact(&mut version_bytes)?;

        let mut hash_bytes = [0; 20];
        reader.read_exact(&mut hash_bytes)?;

        match version_bytes {
            zcash_primitives::constants::mainnet::B58_SCRIPT_ADDRESS_PREFIX => {
                Ok(Address::PayToScriptHash {
                    network: Network::Mainnet,
                    script_hash: hash_bytes,
                })
            }
            zcash_primitives::constants::testnet::B58_SCRIPT_ADDRESS_PREFIX => {
                Ok(Address::PayToScriptHash {
                    network: Network::Testnet,
                    script_hash: hash_bytes,
                })
            }
            zcash_primitives::constants::mainnet::B58_PUBKEY_ADDRESS_PREFIX => {
                Ok(Address::PayToPublicKeyHash {
                    network: Network::Mainnet,
                    pub_key_hash: hash_bytes,
                })
            }
            zcash_primitives::constants::testnet::B58_PUBKEY_ADDRESS_PREFIX => {
                Ok(Address::PayToPublicKeyHash {
                    network: Network::Testnet,
                    pub_key_hash: hash_bytes,
                })
            }
            _ => Err(SerializationError::Parse("bad t-addr version/type")),
        }
    }
}

trait ToAddressWithNetwork {
    /// Convert `self` to an `Address`, given the current `network`.
    fn to_address(&self, network: Network) -> Address;
}

impl ToAddressWithNetwork for Script {
    fn to_address(&self, network: Network) -> Address {
        Address::PayToScriptHash {
            network,
            script_hash: Address::hash_payload(self.as_raw_bytes()),
        }
    }
}

impl ToAddressWithNetwork for PublicKey {
    fn to_address(&self, network: Network) -> Address {
        Address::PayToPublicKeyHash {
            network,
            pub_key_hash: Address::hash_payload(&self.serialize()[..]),
        }
    }
}

impl Address {
    /// Create an address for the given public key hash and network.
    pub fn from_pub_key_hash(network: Network, pub_key_hash: [u8; 20]) -> Self {
        Self::PayToPublicKeyHash {
            network,
            pub_key_hash,
        }
    }

    /// Create an address for the given script hash and network.
    pub fn from_script_hash(network: Network, script_hash: [u8; 20]) -> Self {
        Self::PayToScriptHash {
            network,
            script_hash,
        }
    }

    /// Returns the network for this address.
    pub fn network(&self) -> Network {
        match *self {
            Address::PayToScriptHash { network, .. } => network,
            Address::PayToPublicKeyHash { network, .. } => network,
        }
    }

    /// Returns `true` if the address is `PayToScriptHash`, and `false` if it is `PayToPublicKeyHash`.
    pub fn is_script_hash(&self) -> bool {
        matches!(self, Address::PayToScriptHash { .. })
    }

    /// Returns the hash bytes for this address, regardless of the address type.
    ///
    /// # Correctness
    ///
    /// Use [`ZcashSerialize`] and [`ZcashDeserialize`] for consensus-critical serialization.
    pub fn hash_bytes(&self) -> [u8; 20] {
        match *self {
            Address::PayToScriptHash { script_hash, .. } => script_hash,
            Address::PayToPublicKeyHash { pub_key_hash, .. } => pub_key_hash,
        }
    }

    /// A hash of a transparent address payload, as used in
    /// transparent pay-to-script-hash and pay-to-publickey-hash
    /// addresses.
    ///
    /// The resulting hash in both of these cases is always exactly 20
    /// bytes.
    /// <https://en.bitcoin.it/Base58Check_encoding#Encoding_a_Bitcoin_address>
    fn hash_payload(bytes: &[u8]) -> [u8; 20] {
        let sha_hash = Sha256::digest(bytes);
        let ripe_hash = Ripemd160::digest(sha_hash);
        let mut payload = [0u8; 20];
        payload[..].copy_from_slice(&ripe_hash[..]);
        payload
    }

    /// Given a transparent address (P2SH or a P2PKH), create a script that can be used in a coinbase
    /// transaction output.
    pub fn create_script_from_address(&self) -> Script {
        let mut script_bytes = Vec::new();

        match self {
            // https://developer.bitcoin.org/devguide/transactions.html#pay-to-script-hash-p2sh
            Address::PayToScriptHash { .. } => {
                script_bytes.push(OpCode::Hash160 as u8);
                script_bytes.push(OpCode::Push20Bytes as u8);
                script_bytes.extend(self.hash_bytes());
                script_bytes.push(OpCode::Equal as u8);
            }
            // https://developer.bitcoin.org/devguide/transactions.html#pay-to-public-key-hash-p2pkh
            Address::PayToPublicKeyHash { .. } => {
                script_bytes.push(OpCode::Dup as u8);
                script_bytes.push(OpCode::Hash160 as u8);
                script_bytes.push(OpCode::Push20Bytes as u8);
                script_bytes.extend(self.hash_bytes());
                script_bytes.push(OpCode::EqualVerify as u8);
                script_bytes.push(OpCode::CheckSig as u8);
            }
        };

        Script::new(&script_bytes)
    }
}

#[cfg(test)]
mod tests {

    use secp256k1::PublicKey;

    use super::*;

    #[test]
    fn pubkey_mainnet() {
        let _init_guard = zebra_test::init();

        let pub_key = PublicKey::from_slice(&[
            3, 23, 183, 225, 206, 31, 159, 148, 195, 42, 67, 115, 146, 41, 248, 140, 11, 3, 51, 41,
            111, 180, 110, 143, 114, 134, 88, 73, 198, 174, 52, 184, 78,
        ])
        .expect("A PublicKey from slice");

        let t_addr = pub_key.to_address(Network::Mainnet);

        assert_eq!(format!("{t_addr}"), "t1bmMa1wJDFdbc2TiURQP5BbBz6jHjUBuHq");
    }

    #[test]
    fn pubkey_testnet() {
        let _init_guard = zebra_test::init();

        let pub_key = PublicKey::from_slice(&[
            3, 23, 183, 225, 206, 31, 159, 148, 195, 42, 67, 115, 146, 41, 248, 140, 11, 3, 51, 41,
            111, 180, 110, 143, 114, 134, 88, 73, 198, 174, 52, 184, 78,
        ])
        .expect("A PublicKey from slice");

        let t_addr = pub_key.to_address(Network::Testnet);

        assert_eq!(format!("{t_addr}"), "tmTc6trRhbv96kGfA99i7vrFwb5p7BVFwc3");
    }

    #[test]
    fn empty_script_mainnet() {
        let _init_guard = zebra_test::init();

        let script = Script::new(&[0u8; 20]);

        let t_addr = script.to_address(Network::Mainnet);

        assert_eq!(format!("{t_addr}"), "t3Y5pHwfgHbS6pDjj1HLuMFxhFFip1fcJ6g");
    }

    #[test]
    fn empty_script_testnet() {
        let _init_guard = zebra_test::init();

        let script = Script::new(&[0; 20]);

        let t_addr = script.to_address(Network::Testnet);

        assert_eq!(format!("{t_addr}"), "t2L51LcmpA43UMvKTw2Lwtt9LMjwyqU2V1P");
    }

    #[test]
    fn from_string() {
        let _init_guard = zebra_test::init();

        let t_addr: Address = "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd".parse().unwrap();

        assert_eq!(format!("{t_addr}"), "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd");
    }

    #[test]
    fn debug() {
        let _init_guard = zebra_test::init();

        let t_addr: Address = "t3Vz22vK5z2LcKEdg16Yv4FFneEL1zg9ojd".parse().unwrap();

        assert_eq!(
            format!("{t_addr:?}"),
            "TransparentAddress { network: Mainnet, script_hash: \"7d46a730d31f97b1930d3368a967c309bd4d136a\" }"
        );
    }
}

#[cfg(test)]
proptest! {

    #[test]
    fn transparent_address_roundtrip(taddr in any::<Address>()) {
        let _init_guard = zebra_test::init();

        let mut data = Vec::new();

        taddr.zcash_serialize(&mut data).expect("t-addr should serialize");

        let taddr2 = Address::zcash_deserialize(&data[..]).expect("randomized t-addr should deserialize");

        prop_assert_eq![taddr, taddr2];
    }
}
