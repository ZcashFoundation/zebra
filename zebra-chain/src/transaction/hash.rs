//! Transaction identifiers for Zcash.
//!
//! Zcash has two different transaction identifiers, with different widths:
//! * [`Hash`]: a 32-byte narrow transaction ID, which uniquely identifies mined transactions
//!   (transactions that have been committed to the blockchain in blocks), and
//! * [`WtxId`]: a 64-byte wide transaction ID, which uniquely identifies unmined transactions
//!   (transactions that are sent by wallets or stored in node mempools).
//!
//! Transaction versions 1-4 are uniquely identified by narrow transaction IDs,
//! so Zebra and the Zcash network protocol don't use wide transaction IDs for them.

use std::fmt;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;
use serde::{Deserialize, Serialize};

use crate::serialization::SerializationError;

use super::{txid::TxIdBuilder, AuthDigest, Transaction};

/// A narrow transaction ID, which uniquely identifies mined v5 transactions,
/// and all v1-v4 transactions.
///
/// Note: Zebra displays transaction and block hashes in big-endian byte-order,
/// following the u256 convention set by Bitcoin and zcashd.
#[derive(Copy, Clone, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Hash(pub [u8; 32]);

impl<'a> From<&'a Transaction> for Hash {
    fn from(transaction: &'a Transaction) -> Self {
        let hasher = TxIdBuilder::new(transaction);
        hasher
            .txid()
            .expect("zcash_primitives and Zebra transaction formats must be compatible")
    }
}

impl fmt::Display for Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut reversed_bytes = self.0;
        reversed_bytes.reverse();
        f.write_str(&hex::encode(&reversed_bytes))
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let mut reversed_bytes = self.0;
        reversed_bytes.reverse();
        f.debug_tuple("transaction::Hash")
            .field(&hex::encode(reversed_bytes))
            .finish()
    }
}

impl std::str::FromStr for Hash {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0; 32];
        if hex::decode_to_slice(s, &mut bytes[..]).is_err() {
            Err(SerializationError::Parse("hex decoding error"))
        } else {
            bytes.reverse();
            Ok(Hash(bytes))
        }
    }
}

/// A wide transaction ID, which uniquely identifies unmined v5 transactions.
///
/// Wide transaction IDs are not used for transaction versions 1-4.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct WtxId {
    /// The non-malleable transaction ID for this transaction's effects.
    pub id: Hash,

    /// The authorizing data digest for this transactions signatures, proofs, and scripts.
    pub auth_digest: AuthDigest,
}

impl<'a> From<&'a Transaction> for WtxId {
    /// Computes the wide transaction ID for a transaction.
    ///
    /// # Panics
    ///
    /// If passed a pre-v5 transaction.
    fn from(transaction: &'a Transaction) -> Self {
        Self {
            id: transaction.into(),
            auth_digest: transaction.into(),
        }
    }
}

impl fmt::Display for WtxId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.id.to_string())?;
        f.write_str(&self.auth_digest.to_string())
    }
}

impl std::str::FromStr for WtxId {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // we need to split using bytes,
        // because str::split_at panics if it splits a UTF-8 codepoint
        let s = s.as_bytes();

        if s.len() == 128 {
            let (id, auth_digest) = s.split_at(64);
            let id = std::str::from_utf8(id)?;
            let auth_digest = std::str::from_utf8(auth_digest)?;

            Ok(Self {
                id: id.parse()?,
                auth_digest: auth_digest.parse()?,
            })
        } else {
            Err(SerializationError::Parse(
                "wrong length for WtxId hex string",
            ))
        }
    }
}
