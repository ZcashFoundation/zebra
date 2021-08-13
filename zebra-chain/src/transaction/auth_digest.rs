use std::{borrow::Borrow, fmt};

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use crate::{
    primitives::zcash_primitives::auth_digest,
    serialization::{
        ReadZcashExt, SerializationError, WriteZcashExt, ZcashDeserialize, ZcashSerialize,
    },
};

use super::Transaction;

/// An authorizing data commitment hash as specified in [ZIP-244].
///
/// Note: Zebra displays transaction and block hashes in big-endian byte-order,
/// following the u256 convention set by Bitcoin and zcashd.
///
/// [ZIP-244]: https://zips.z.cash/zip-0244
#[derive(Copy, Clone, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct AuthDigest(pub(crate) [u8; 32]);

impl<Tx> From<Tx> for AuthDigest
where
    Tx: Borrow<Transaction>,
{
    /// Computes the authorizing data commitment for a transaction.
    ///
    /// # Panics
    ///
    /// If passed a pre-v5 transaction.
    fn from(transaction: Tx) -> Self {
        auth_digest(transaction.borrow())
    }
}

impl From<[u8; 32]> for AuthDigest {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
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

impl fmt::Display for AuthDigest {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut reversed_bytes = self.0;
        reversed_bytes.reverse();
        f.write_str(&hex::encode(&reversed_bytes))
    }
}

impl fmt::Debug for AuthDigest {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut reversed_bytes = self.0;
        reversed_bytes.reverse();
        f.debug_tuple("AuthDigest")
            .field(&hex::encode(reversed_bytes))
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
