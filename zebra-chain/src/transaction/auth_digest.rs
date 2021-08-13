use std::fmt;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use crate::{primitives::zcash_primitives::auth_digest, serialization::SerializationError};

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

impl<'a> From<&'a Transaction> for AuthDigest {
    /// Computes the authorizing data commitment for a transaction.
    ///
    /// # Panics
    ///
    /// If passed a pre-v5 transaction.
    fn from(transaction: &'a Transaction) -> Self {
        auth_digest(transaction)
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
