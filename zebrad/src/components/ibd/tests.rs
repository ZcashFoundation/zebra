//! Known-hash IBD engine tests

use std::sync::Arc;

use zebra_chain::{block, serialization::ZcashDeserializeInto};

use super::engine::HashSource;
use crate::BoxError;

mod cache;
mod convert_vectors;
mod semantic_vectors;

/// An in-memory [`HashSource`] over real test-vector blocks, so engine tests
/// don't read (and re-hash) the ~103 MB on-disk asset set.
///
/// Heights map onto [`zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS`]: the
/// list's hash for height `h` is the real mainnet block `h`'s hash, so mock
/// peers can serve blocks the batcher's hash matching accepts.
pub(crate) struct FakeHashList {
    /// The pinned hashes, indexed by height.
    hashes: Vec<block::Hash>,
}

impl FakeHashList {
    /// Returns a list over the first `len` continuous mainnet test blocks,
    /// alongside the blocks themselves.
    pub(crate) fn continuous_mainnet(len: usize) -> (Self, Vec<Arc<block::Block>>) {
        let blocks: Vec<Arc<block::Block>> = zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS
            .values()
            .take(len)
            .map(|bytes| {
                bytes
                    .zcash_deserialize_into()
                    .expect("test vectors deserialize")
            })
            .collect();

        assert_eq!(
            blocks.len(),
            len,
            "not enough continuous mainnet test vectors",
        );

        let list = Self {
            hashes: blocks.iter().map(|block| block.hash()).collect(),
        };

        (list, blocks)
    }
}

impl HashSource for FakeHashList {
    fn max_height(&self) -> block::Height {
        assert!(!self.hashes.is_empty(), "test lists are never empty");
        // test lists are tiny, heights fit u32
        block::Height((self.hashes.len() - 1) as u32)
    }

    fn hash(&mut self, height: block::Height) -> Result<Option<block::Hash>, BoxError> {
        Ok(self.hashes.get(height.0 as usize).copied())
    }
}
