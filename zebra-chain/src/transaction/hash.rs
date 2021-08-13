//! Transaction identifiers for Zcash.
//!
//! Zcash has two different transaction identifiers, with different widths:
//! * a 32-byte narrow transaction ID, which uniquely identifies mined transactions
//!   (transactions that have been committed to the blockchain in blocks), and
//! * a 64-byte wide transaction ID, which uniquely identifies unmined transactions
//!   (transactions that are sent by wallets or stored in node mempools).
//!
//! Transaction versions 1-4 are uniquely identified by narrow transaction IDs,
//! so Zebra and the Zcash network protocol don't use wide transaction IDs for them.

use std::fmt;

#[cfg(any(test, feature = "proptest-impl"))]
use proptest_derive::Arbitrary;
use serde::{Deserialize, Serialize};

use crate::serialization::SerializationError;

use super::{txid::TxIdBuilder, Transaction};

/// A narrow transaction ID, which uniquely identifies mined transactions
/// (transactions that have been committed to a block in the blockchain).
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
