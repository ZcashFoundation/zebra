//! A binary hash tree of SHA256d (two rounds of SHA256) hashes for
//! node values.

use std::io;
use std::io::prelude::*;

use sha2::{Digest, Sha256};

use crate::serialization::{SerializationError, ZcashDeserialize, ZcashSerialize};

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

impl<T> ZcashSerialize for MerkleTree<T> {
    fn zcash_serialize<W: io::Write>(&self, writer: W) -> Result<(), SerializationError> {
        unimplemented!();
    }
}

impl<T> ZcashDeserialize for MerkleTree<T> {
    fn zcash_deserialize<R: io::Read>(reader: R) -> Result<Self, SerializationError> {
        unimplemented!();
    }
}
