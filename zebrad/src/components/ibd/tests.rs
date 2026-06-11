//! Known-hash IBD engine tests

use std::{collections::BTreeMap, sync::Arc};

use zebra_chain::{block, serialization::ZcashDeserializeInto};

use super::engine::HashList;
use crate::BoxError;

mod convert_vectors;
mod engine_prop;
mod vectors;

/// An in-memory [`HashList`] over real test-vector blocks, so engine tests
/// don't read (and re-hash) the ~103 MB on-disk asset set.
///
/// Heights map onto [`zebra_test::vectors::CONTINUOUS_MAINNET_BLOCKS`]: the
/// list's hash for height `h` is the real mainnet block `h`'s hash, so mock
/// peers can serve blocks the batcher's hash matching accepts.
pub(crate) struct FakeHashList {
    /// The pinned hashes, indexed by height.
    hashes: Vec<block::Hash>,

    /// Per-height size-hint overrides; other heights use the trait default.
    hints: BTreeMap<u32, u8>,
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
            hints: BTreeMap::new(),
        };

        (list, blocks)
    }

    /// Sets the size hint for every height in the list.
    pub(crate) fn with_uniform_hint(self, hint: u8) -> Self {
        let len = self.hashes.len();
        self.with_hints(&vec![hint; len])
    }

    /// Sets per-height size hints, starting at height 0.
    pub(crate) fn with_hints(mut self, hints: &[u8]) -> Self {
        for (height, hint) in hints.iter().enumerate() {
            // test lists are tiny, heights fit u32
            self.hints.insert(height as u32, *hint);
        }
        self
    }
}

impl HashList for FakeHashList {
    fn max_height(&self) -> block::Height {
        assert!(!self.hashes.is_empty(), "test lists are never empty");
        // test lists are tiny, heights fit u32
        block::Height((self.hashes.len() - 1) as u32)
    }

    fn hash(&mut self, height: block::Height) -> Result<Option<block::Hash>, BoxError> {
        Ok(self.hashes.get(height.0 as usize).copied())
    }

    fn size_hint(&mut self, height: block::Height) -> u8 {
        self.hints
            .get(&height.0)
            .copied()
            .unwrap_or(super::fetch::DEFAULT_SIZE_HINT)
    }
}
