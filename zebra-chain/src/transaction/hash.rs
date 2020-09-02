#![allow(clippy::unit_arg)]
use std::fmt;

#[cfg(test)]
use proptest_derive::Arbitrary;
use serde::{Deserialize, Serialize};

use crate::serialization::{sha256d, SerializationError, ZcashSerialize};

use super::Transaction;

/// A hash of a `Transaction`
///
/// TODO: I'm pretty sure this is also a SHA256d hash but I haven't
/// confirmed it yet.
#[derive(Copy, Clone, Eq, PartialEq, Serialize, Deserialize, Hash)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct Hash(pub [u8; 32]);

impl<'a> From<&'a Transaction> for Hash {
    fn from(transaction: &'a Transaction) -> Self {
        let mut hash_writer = sha256d::Writer::default();
        transaction
            .zcash_serialize(&mut hash_writer)
            .expect("Transactions must serialize into the hash.");
        Self(hash_writer.finish())
    }
}

impl fmt::Debug for Hash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("TransactionHash")
            .field(&hex::encode(&self.0))
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
            Ok(Hash(bytes))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transactionhash_from_str() {
        let hash: Hash = "bf46b4b5030752fedac6f884976162bbfb29a9398f104a280b3e34d51b416631"
            .parse()
            .unwrap();
        assert_eq!(
            format!("{:?}", hash),
            r#"TransactionHash("bf46b4b5030752fedac6f884976162bbfb29a9398f104a280b3e34d51b416631")"#
        );
    }
}
