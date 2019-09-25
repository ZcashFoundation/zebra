//! A binary hash tree of SHA256d (two rounds of SHA256) hashes for
//! node values.

use sha2::Sha256;

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
