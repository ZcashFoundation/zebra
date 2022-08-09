//! Sprout key types
//!
//! "The receiving key sk_enc, the incoming viewing key ivk = (apk,
//! sk_enc), and the shielded payment address addr_pk = (a_pk, pk_enc) are
//! derived from a_sk, as described in ['Sprout Key Components'][ps]
//!
//! [ps]: https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents
#![allow(clippy::unit_arg)]

use std::{fmt, io};

use byteorder::{ByteOrder, LittleEndian};
use rand_core::{CryptoRng, RngCore};
use sha2::digest::generic_array::{typenum::U64, GenericArray};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest::{array, prelude::*};
#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use crate::{
    parameters::Network,
    serialization::{ReadZcashExt, SerializationError, ZcashDeserialize, ZcashSerialize},
};

/// Magic numbers used to identify with what networks Sprout Spending
/// Keys are associated.
mod sk_magics {
    pub const MAINNET: [u8; 2] = [0xAB, 0x36];
    pub const TESTNET: [u8; 2] = [0xAC, 0x08];
}

/// PRF^addr is used to derive a Sprout shielded payment address from
/// a spending key, and instantiated using the SHA-256 compression
/// function.
///
/// <https://zips.z.cash/protocol/protocol.pdf#abstractprfs>
/// <https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents>
fn prf_addr(x: [u8; 32], t: u8) -> [u8; 32] {
    let mut state = [0u32; 8];
    let mut block = GenericArray::<u8, U64>::default();

    block.as_mut_slice()[0..32].copy_from_slice(&x[..]);
    // The first four bits –i.e. the most signicant four bits of the
    // first byte– are used to separate distinct uses
    // of SHA256Compress, ensuring that the functions are independent.
    block.as_mut_slice()[0] |= 0b1100_0000;

    block.as_mut_slice()[32] = t;

    sha2::compress256(&mut state, &[block]);

    let mut derived_bytes = [0u8; 32];
    LittleEndian::write_u32_into(&state, &mut derived_bytes);

    derived_bytes
}

/// Our root secret key of the Sprout key derivation tree.
///
/// All other Sprout key types derive from the SpendingKey value.
/// Actually 252 bits.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct SpendingKey {
    /// What would normally be the value inside a tuple struct.
    pub bytes: [u8; 32],
    /// The Zcash network with which this key is associated.
    pub network: Network,
}

impl ZcashSerialize for SpendingKey {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self.network {
            Network::Mainnet => writer.write_all(&sk_magics::MAINNET[..])?,
            _ => writer.write_all(&sk_magics::TESTNET[..])?,
        }
        writer.write_all(&self.bytes[..])?;

        Ok(())
    }
}

impl ZcashDeserialize for SpendingKey {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut version_bytes = [0; 2];
        reader.read_exact(&mut version_bytes)?;

        let network = match version_bytes {
            sk_magics::MAINNET => Network::Mainnet,
            sk_magics::TESTNET => Network::Testnet,
            _ => panic!("SerializationError: bad sprout spending key version/type"),
        };

        Ok(SpendingKey {
            network,
            bytes: reader.read_32_bytes()?,
        })
    }
}

impl fmt::Display for SpendingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut bytes = io::Cursor::new(Vec::new());

        let _ = self.zcash_serialize(&mut bytes);

        f.write_str(&bs58::encode(bytes.get_ref()).with_check().into_string())
    }
}

impl From<[u8; 32]> for SpendingKey {
    /// Generate a _SpendingKey_ from existing bytes, with the high 4
    /// bits of the first byte set to zero (ie, 256 bits clamped to
    /// 252).
    fn from(mut bytes: [u8; 32]) -> SpendingKey {
        bytes[0] &= 0b0000_1111; // Force the 4 high-order bits to zero.

        SpendingKey {
            bytes,
            network: Network::default(),
        }
    }
}

impl From<SpendingKey> for [u8; 32] {
    fn from(spending_key: SpendingKey) -> [u8; 32] {
        spending_key.bytes
    }
}

impl<'a> From<&'a SpendingKey> for [u8; 32] {
    fn from(spending_key: &'a SpendingKey) -> [u8; 32] {
        spending_key.bytes
    }
}

impl std::str::FromStr for SpendingKey {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let result = &bs58::decode(s).with_check(None).into_vec();

        match result {
            Ok(bytes) => Self::zcash_deserialize(&bytes[..]),
            Err(_) => Err(SerializationError::Parse("bs58 decoding error")),
        }
    }
}

impl SpendingKey {
    /// Generate a new _SpendingKey_ with the high 4 bits of the first
    /// byte set to zero (ie, 256 random bits, clamped to 252).
    pub fn new<T>(csprng: &mut T) -> Self
    where
        T: RngCore + CryptoRng,
    {
        let mut bytes = [0u8; 32];
        csprng.fill_bytes(&mut bytes);

        Self::from(bytes)
    }
}

/// Derived from a _SpendingKey_.
pub type ReceivingKey = x25519_dalek::StaticSecret;

impl From<SpendingKey> for ReceivingKey {
    /// For this invocation of SHA256Compress as PRF^addr, t=0, which
    /// is populated by default in an empty block of all zeros to
    /// start.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents>
    /// <https://zips.z.cash/protocol/protocol.pdf#concreteprfs>
    fn from(spending_key: SpendingKey) -> ReceivingKey {
        let derived_bytes = prf_addr(spending_key.bytes, 0);

        ReceivingKey::from(derived_bytes)
    }
}

/// Derived from a _SpendingKey_.
#[derive(Copy, Clone, Eq, PartialEq)]
#[cfg_attr(
    any(test, feature = "proptest-impl"),
    derive(proptest_derive::Arbitrary)
)]
pub struct PayingKey(pub [u8; 32]);

impl AsRef<[u8]> for PayingKey {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for PayingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("PayingKey")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl From<SpendingKey> for PayingKey {
    /// For this invocation of SHA256Compress as PRF^addr, t=1.
    ///
    /// <https://zips.z.cash/protocol/protocol.pdf#sproutkeycomponents>
    /// <https://zips.z.cash/protocol/protocol.pdf#concreteprfs>
    fn from(spending_key: SpendingKey) -> PayingKey {
        let derived_bytes = prf_addr(spending_key.bytes, 1);

        PayingKey(derived_bytes)
    }
}

/// Derived from a _ReceivingKey_.
pub type TransmissionKey = x25519_dalek::PublicKey;

/// Magic numbers used to identify with what networks Sprout Incoming
/// Viewing Keys are associated.
mod ivk_magics {
    pub const MAINNET: [u8; 3] = [0xA8, 0xAB, 0xD3];
    pub const TESTNET: [u8; 3] = [0xA8, 0xAC, 0x0C];
}

/// The recipient's possession of the associated incoming viewing key
/// is used to reconstruct the original note and memo field.
pub struct IncomingViewingKey {
    network: Network,
    paying_key: PayingKey,
    receiving_key: ReceivingKey,
}

// Can't derive PartialEq because ReceivingKey aka
// x25519_dalek::StaticSecret does not impl it.
impl PartialEq for IncomingViewingKey {
    fn eq(&self, other: &Self) -> bool {
        self.network == other.network
            && self.paying_key.0 == other.paying_key.0
            && self.receiving_key.to_bytes() == other.receiving_key.to_bytes()
    }
}

impl fmt::Debug for IncomingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("IncomingViewingKey")
            .field("network", &self.network)
            .field("paying_key", &hex::encode(&self.paying_key.0))
            .field(
                "receiving_key",
                &hex::encode(&self.receiving_key.to_bytes()),
            )
            .finish()
    }
}

impl ZcashSerialize for IncomingViewingKey {
    fn zcash_serialize<W: io::Write>(&self, mut writer: W) -> Result<(), io::Error> {
        match self.network {
            Network::Mainnet => writer.write_all(&ivk_magics::MAINNET[..])?,
            _ => writer.write_all(&ivk_magics::TESTNET[..])?,
        }
        writer.write_all(&self.paying_key.0[..])?;
        writer.write_all(&self.receiving_key.to_bytes())?;

        Ok(())
    }
}

impl ZcashDeserialize for IncomingViewingKey {
    fn zcash_deserialize<R: io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        let mut version_bytes = [0; 3];
        reader.read_exact(&mut version_bytes)?;

        let network = match version_bytes {
            ivk_magics::MAINNET => Network::Mainnet,
            ivk_magics::TESTNET => Network::Testnet,
            _ => panic!("SerializationError: bad sprout incoming viewing key network"),
        };

        Ok(IncomingViewingKey {
            network,
            paying_key: PayingKey(reader.read_32_bytes()?),
            receiving_key: ReceivingKey::from(reader.read_32_bytes()?),
        })
    }
}

impl fmt::Display for IncomingViewingKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut bytes = io::Cursor::new(Vec::new());

        let _ = self.zcash_serialize(&mut bytes);

        f.write_str(&bs58::encode(bytes.get_ref()).with_check().into_string())
    }
}

impl std::str::FromStr for IncomingViewingKey {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let result = &bs58::decode(s).with_check(None).into_vec();

        match result {
            Ok(bytes) => Self::zcash_deserialize(&bytes[..]),
            Err(_) => Err(SerializationError::Parse("bs58 decoding error")),
        }
    }
}

#[cfg(any(test, feature = "proptest-impl"))]
impl Arbitrary for IncomingViewingKey {
    type Parameters = ();

    fn arbitrary_with(_args: Self::Parameters) -> Self::Strategy {
        (
            any::<Network>(),
            array::uniform32(any::<u8>()),
            array::uniform32(any::<u8>()),
        )
            .prop_map(|(network, paying_key_bytes, receiving_key_bytes)| Self {
                network,
                paying_key: PayingKey(paying_key_bytes),
                receiving_key: ReceivingKey::from(receiving_key_bytes),
            })
            .boxed()
    }

    type Strategy = BoxedStrategy<Self>;
}

#[cfg(test)]
mod tests {

    use rand_core::OsRng;

    use super::*;

    #[test]
    // TODO: test vectors, not just random data
    fn derive_keys() {
        let _init_guard = zebra_test::init();

        let spending_key = SpendingKey::new(&mut OsRng);

        let receiving_key = ReceivingKey::from(spending_key);

        let _transmission_key = TransmissionKey::from(&receiving_key);
    }
}

#[cfg(test)]
proptest! {

    #[test]
    fn spending_key_roundtrip(sk in any::<SpendingKey>()) {
        let _init_guard = zebra_test::init();

        let mut data = Vec::new();

        sk.zcash_serialize(&mut data).expect("sprout spending key should serialize");

        let sk2 = SpendingKey::zcash_deserialize(&data[..]).expect("randomized sprout spending key should deserialize");

        prop_assert_eq![sk, sk2];

    }

    #[test]
    fn spending_key_string_roundtrip(sk in any::<SpendingKey>()) {
        let _init_guard = zebra_test::init();

        let string = sk.to_string();

        let sk2 = string.parse::<SpendingKey>().unwrap();

        prop_assert_eq![sk, sk2];

    }

    #[test]
    fn incoming_viewing_key_roundtrip(ivk in any::<IncomingViewingKey>()) {
        let _init_guard = zebra_test::init();

        let mut data = Vec::new();

        ivk.zcash_serialize(&mut data).expect("sprout z-addr should serialize");

        let ivk2 = IncomingViewingKey::zcash_deserialize(&data[..]).expect("randomized ivk should deserialize");

        prop_assert_eq![ivk, ivk2];

    }

    #[test]
    fn incoming_viewing_key_string_roundtrip(ivk in any::<IncomingViewingKey>()) {
        let _init_guard = zebra_test::init();

        let string = ivk.to_string();

        let ivk2 = string.parse::<IncomingViewingKey>().unwrap();

        prop_assert_eq![ivk, ivk2];

    }
}
