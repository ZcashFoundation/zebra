use crate::{serialization::ZcashSerialize, sha256d_writer::Sha256dWriter};

use super::Transaction;

/// A hash of a `Transaction`
///
/// TODO: I'm pretty sure this is also a SHA256d hash but I haven't
/// confirmed it yet.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
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
