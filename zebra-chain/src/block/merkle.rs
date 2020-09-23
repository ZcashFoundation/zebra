//! The Bitcoin-inherited Merkle tree of transactions.
#![allow(clippy::unit_arg)]

use std::{fmt, io};

#[cfg(any(any(test, feature = "proptest-impl"), feature = "proptest-impl"))]
use proptest_derive::Arbitrary;

use crate::serialization::{sha256d, SerializationError, ZcashDeserialize, ZcashSerialize};
use crate::transaction::Transaction;

/// A binary hash tree of SHA256d (two rounds of SHA256) hashes for
/// node values.
#[derive(Default)]
pub struct Tree<T> {
    _leaves: Vec<T>,
}

impl<Transaction> ZcashSerialize for Tree<Transaction> {
    fn zcash_serialize<W: io::Write>(&self, _writer: W) -> Result<(), io::Error> {
        unimplemented!();
    }
}

impl<Transaction> ZcashDeserialize for Tree<Transaction> {
    fn zcash_deserialize<R: io::Read>(_reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}

/// A SHA-256d hash of the root node of a merkle tree of SHA256-d
/// hashed transactions in a block.
#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[cfg_attr(any(test, feature = "proptest-impl"), derive(Arbitrary))]
pub struct Root(pub [u8; 32]);

impl From<Tree<Transaction>> for Root {
    fn from(merkle_tree: Tree<Transaction>) -> Self {
        let mut hash_writer = sha256d::Writer::default();
        merkle_tree
            .zcash_serialize(&mut hash_writer)
            .expect("Sha256dWriter is infallible");
        Self(hash_writer.finish())
    }
}

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Root").field(&hex::encode(&self.0)).finish()
    }
}
