#![allow(clippy::unit_arg)]
use std::fmt;

#[cfg(test)]
use proptest_derive::Arbitrary;
use serde::{Deserialize, Serialize};

use crate::{
    serialization::{SerializationError, ZcashSerialize},
    sha256d_writer::Sha256dWriter,
};

use super::Transaction;

/// A hash of a `Transaction`
///
/// TODO: I'm pretty sure this is also a SHA256d hash but I haven't
/// confirmed it yet.
#[derive(Copy, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct TransactionHash(pub [u8; 32]);

impl From<Transaction> for TransactionHash {
    fn from(transaction: Transaction) -> Self {
        let mut hash_writer = Sha256dWriter::default();
        transaction
            .zcash_serialize(&mut hash_writer)
            .expect("Transactions must serialize into the hash.");
        Self(hash_writer.finish())
    }
}

impl fmt::Debug for TransactionHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("TransactionHash")
            .field(&hex::encode(&self.0))
            .finish()
    }
}

impl std::str::FromStr for TransactionHash {
    type Err = SerializationError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0; 32];
        if hex::decode_to_slice(s, &mut bytes[..]).is_err() {
            Err(SerializationError::Parse("hex decoding error"))
        } else {
            Ok(TransactionHash(bytes))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use crate::sha256d_writer::Sha256dWriter;

    use super::*;

    #[test]
    fn transactionhash_debug() {
        let preimage = b"foo bar baz";
        let mut sha_writer = Sha256dWriter::default();
        let _ = sha_writer.write_all(preimage);

        let hash = TransactionHash(sha_writer.finish());

        assert_eq!(
            format!("{:?}", hash),
            r#"TransactionHash("bf46b4b5030752fedac6f884976162bbfb29a9398f104a280b3e34d51b416631")"#
        );
    }

    #[test]
    fn transactionhash_from_str() {
        let hash: TransactionHash =
            "bf46b4b5030752fedac6f884976162bbfb29a9398f104a280b3e34d51b416631"
                .parse()
                .unwrap();
        assert_eq!(
            format!("{:?}", hash),
            r#"TransactionHash("bf46b4b5030752fedac6f884976162bbfb29a9398f104a280b3e34d51b416631")"#
        );
    }
}
