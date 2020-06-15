//! A binary hash tree of SHA256d (two rounds of SHA256) hashes for
//! node values.
#![allow(clippy::unit_arg)]

use std::{fmt, io};

#[cfg(test)]
use proptest_derive::Arbitrary;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};
use crate::sha256d_writer::Sha256dWriter;
use crate::transaction::Transaction;

/// A binary hash tree of SHA256d (two rounds of SHA256) hashes for
/// node values.
#[derive(Default)]
pub struct MerkleTree<T> {
    _leaves: Vec<T>,
}

impl<Transaction> ZcashSerialize for MerkleTree<Transaction> {
    fn zcash_serialize<W: io::Write>(&self, _writer: W) -> Result<(), io::Error> {
        unimplemented!();
    }
}

impl<Transaction> ZcashDeserialize for MerkleTree<Transaction> {
    fn zcash_deserialize<R: io::Read>(_reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}

/// A SHA-256d hash of the root node of a merkle tree of SHA256-d
/// hashed transactions in a block.
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(Arbitrary))]
pub struct MerkleTreeRootHash(pub [u8; 32]);

impl From<MerkleTree<Transaction>> for MerkleTreeRootHash {
    fn from(merkle_tree: MerkleTree<Transaction>) -> Self {
        let mut hash_writer = Sha256dWriter::default();
        merkle_tree
            .zcash_serialize(&mut hash_writer)
            .expect("Sha256dWriter is infallible");
        Self(hash_writer.finish())
    }
}

impl fmt::Debug for MerkleTreeRootHash {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("MerkleTreeRootHash")
            .field(&hex::encode(&self.0))
            .finish()
    }
}
