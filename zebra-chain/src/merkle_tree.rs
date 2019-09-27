//! A binary hash tree of SHA256d (two rounds of SHA256) hashes for
//! node values.

use std::io;

use sha2::Sha256;

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};
use crate::sha256d_writer::Sha256dWriter;
use crate::transaction::Transaction;

/// A binary hash tree of SHA256d (two rounds of SHA256) hashes for
/// node values.
#[derive(Default)]
pub struct MerkleTree<T> {
    leaves: Vec<T>,
}

impl<T> MerkleTree<T> {
    pub fn get_root(&self) -> Sha256 {
        unimplemented!();
    }
}

impl<Transaction> ZcashSerialize for MerkleTree<Transaction> {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), SerializationError> {
        unimplemented!();
    }
}

impl<Transaction> ZcashDeserialize for MerkleTree<Transaction> {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}

/// A SHA-256d hash of the root node of a merkle tree of SHA256-d
/// hashed transactions in a block.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MerkleTreeRootHash([u8; 32]);

impl From<MerkleTree<Transaction>> for MerkleTreeRootHash {
    fn from(merkle_tree: MerkleTree<Transaction>) -> Self {
        let mut hash_writer = Sha256dWriter::default();
        merkle_tree
            .zcash_serialize(&mut hash_writer)
            .expect("The merkle tree of transactions must serialize.");
        Self(hash_writer.finish())
    }
}
