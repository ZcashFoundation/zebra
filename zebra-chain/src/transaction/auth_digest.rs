//! Authorizing digests for Zcash transactions.

use std::{array::TryFromSliceError, fmt, sync::Arc};

use hex::{FromHex, ToHex};

use crate::{
    primitives::zcash_primitives::auth_digest,
    serialization::{
        ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
    },
};

use super::Transaction;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

/// An authorizing data commitment hash as specified in [ZIP-244].
///
/// Note: Zebra displays transaction and block hashes in big-endian byte-order,
/// following the u256 convention set by Bitcoin and zcashd.
///
/// [ZIP-244]: https://zips.z.cash/zip-0244
#[derive(Copy, Clone, Eq, PartialEq, Hash, serde::Deserialize, serde::Serialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct AuthDigest(pub [u8; 32]);

impl AuthDigest {
    /// Return the hash bytes in big-endian byte-order suitable for printing out byte by byte.
    ///
    /// Zebra displays transaction and block hashes in big-endian byte-order,
    /// following the u256 convention set by Bitcoin and zcashd.
    pub fn bytes_in_display_order(&self) -> [u8; 32] {
        let mut reversed_bytes = self.0;
        reversed_bytes.reverse();
        reversed_bytes
    }

    /// Convert bytes in big-endian byte-order into a [`transaction::AuthDigest`](crate::transaction::AuthDigest).
    ///
    /// Zebra displays transaction and block hashes in big-endian byte-order,
    /// following the u256 convention set by Bitcoin and zcashd.
    pub fn from_bytes_in_display_order(bytes_in_display_order: &[u8; 32]) -> AuthDigest {
        let mut internal_byte_order = *bytes_in_display_order;
        internal_byte_order.reverse();

        AuthDigest(internal_byte_order)
    }
}

impl From<Transaction> for AuthDigest {
    /// Computes the authorizing data commitment for a transaction.
    ///
    /// # Panics
    ///
    /// If passed a pre-v5 transaction.
    fn from(transaction: Transaction) -> Self {
        // use the ref implementation, to avoid cloning the transaction
        AuthDigest::from(&transaction)
    }
}

impl From<&Transaction> for AuthDigest {
    /// Computes the authorizing data commitment for a transaction.
    ///
    /// # Panics
    ///
    /// If passed a pre-v5 transaction.
    fn from(transaction: &Transaction) -> Self {
        auth_digest(transaction)
    }
}

impl From<Arc<Transaction>> for AuthDigest {
    /// Computes the authorizing data commitment for a transaction.
    ///
    /// # Panics
    ///
    /// If passed a pre-v5 transaction.
    fn from(transaction: Arc<Transaction>) -> Self {
        transaction.as_ref().into()
    }
}

impl From<[u8; 32]> for AuthDigest {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl TryFrom<&[u8]> for AuthDigest {
    type Error = TryFromSliceError;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Ok(AuthDigest(bytes.try_into()?))
    }
}

impl From<AuthDigest> for [u8; 32] {
    fn from(auth_digest: AuthDigest) -> Self {
        auth_digest.0
    }
}

impl From<&AuthDigest> for [u8; 32] {
    fn from(auth_digest: &AuthDigest) -> Self {
        (*auth_digest).into()
    }
}

impl ToHex for &AuthDigest {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        self.bytes_in_display_order().encode_hex_upper()
    }
}

impl ToHex for AuthDigest {
    fn encode_hex<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex()
    }

    fn encode_hex_upper<T: FromIterator<char>>(&self) -> T {
        (&self).encode_hex_upper()
    }
}

impl FromHex for AuthDigest {
    type Error = <[u8; 32] as FromHex>::Error;

    fn from_hex<T: AsRef<[u8]>>(hex: T) -> Result<Self, Self::Error> {
        let mut hash = <[u8; 32]>::from_hex(hex)?;
        hash.reverse();

        Ok(hash.into())
    }
}

impl fmt::Display for AuthDigest {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.encode_hex::<String>())
    }
}

impl fmt::Debug for AuthDigest {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("AuthDigest")
            .field(&self.encode_hex::<String>())
            .finish()
    }
}

impl std::str::FromStr for AuthDigest {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0; 32];
        if hex::decode_to_slice(s, &mut bytes[..]).is_err() {
            Err(SerializationError::Parse("hex decoding error"))
        } else {
            bytes.reverse();
            Ok(AuthDigest(bytes))
        }
    }
}

impl ZcashSerialize for AuthDigest {
    fn zcash_serialize<W: std::io::Write>(&self, mut writer: W) -> Result<(), std::io::Error> {
        writer.write_32_bytes(&self.into())
    }
}

impl ZcashDeserialize for AuthDigest {
    fn zcash_deserialize<R: std::io::Read>(mut reader: R) -> Result<Self, SerializationError> {
        Ok(reader.read_32_bytes()?.into())
    }
}
