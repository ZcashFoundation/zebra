//! Reading a block's ancestor chain, for serving version 2 `get-block-range`
//! requests.

use std::sync::Arc;

use zebra_chain::{
    block::{self, Block},
    serialization::ZcashSerialize,
};

use crate::{
    service::{finalized_state::ZebraDb, non_finalized_state::Chain},
    HashOrHeight,
};

#[cfg(test)]
mod tests;

/// The most blocks reserved in the result up front, so that an untrusted
/// `count` can't force a large allocation before any block is found.
const MAX_PREALLOCATED_BLOCKS: usize = 1024;

/// Returns up to `count` blocks of the ancestor chain of `hash`, highest
/// first, in any non-finalized chain or the finalized state.
///
/// See [`ReadRequest::AncestorBlocks`](crate::ReadRequest::AncestorBlocks)
/// for the walk and the bounds. Runs in the read service's blocking pool.
pub fn ancestor_blocks<'a, C: AsRef<Chain> + 'a>(
    chains: impl Iterator<Item = &'a C>,
    db: &ZebraDb,
    hash: block::Hash,
    count: u32,
    max_bytes: u64,
) -> Vec<Arc<Block>> {
    // `count` is a `u32`, and `usize` is at least 32 bits on supported platforms.
    let count = count as usize;
    let mut walk = Walk {
        blocks: Vec::with_capacity(count.min(MAX_PREALLOCATED_BLOCKS)),
        count,
        max_bytes,
        total_bytes: 0,
    };

    if count == 0 {
        return walk.blocks;
    }

    // The hash of the next block to add: the requested block, then the
    // parent of the last block added.
    let mut next_hash = hash;

    // Every chain holds all the blocks from just above the finalized tip up
    // to its own tip, so the first chain that has `hash` also has all of its
    // non-finalized ancestors.
    if let Some((chain, height)) = chains
        .map(AsRef::as_ref)
        .find_map(|chain| chain.height_by_hash(hash).map(|height| (chain, height)))
    {
        for contextual in chain.blocks.range(..=height).rev().map(|(_, block)| block) {
            if contextual.hash != next_hash {
                // A chain's blocks always form a hash chain, so this can't
                // happen; stop rather than serve a block that doesn't link.
                return walk.blocks;
            }

            let size = chain
                .block_info(contextual.height.into())
                .map(|info| u64::from(info.size()))
                .unwrap_or_else(|| {
                    // A `usize` always fits in a `u64` on supported platforms.
                    contextual.block.zcash_serialized_size() as u64
                });

            if !walk.push(contextual.block.clone(), size) || contextual.height == block::Height(0) {
                return walk.blocks;
            }
            next_hash = contextual.block.header.previous_block_hash;
        }
    }

    // Switch to the finalized state, in height order. Looking the entry up by
    // hash checks the hash at the entry height, and also covers a finalized
    // tip that has moved past the in-memory chains since they were read.
    let Some(entry_height) = db.height(next_hash) else {
        return walk.blocks;
    };

    for height in (0..=entry_height.0).rev() {
        // A missing body ends the walk early, which the protocol allows.
        let Some((block, size)) = db.block_and_size(HashOrHeight::Height(block::Height(height)))
        else {
            break;
        };

        debug_assert_eq!(
            block.hash(),
            next_hash,
            "the finalized state is a single hash chain"
        );
        next_hash = block.header.previous_block_hash;

        // A `usize` always fits in a `u64` on supported platforms.
        if !walk.push(block, size as u64) {
            break;
        }
    }

    walk.blocks
}

/// The blocks found so far, and the bounds on adding more.
struct Walk {
    blocks: Vec<Arc<Block>>,
    count: usize,
    max_bytes: u64,
    total_bytes: u64,
}

impl Walk {
    /// Adds `block` of serialized `size` if the bounds allow it, and returns
    /// whether the walk should continue.
    ///
    /// The first block is always added, whatever its size.
    fn push(&mut self, block: Arc<Block>, size: u64) -> bool {
        let total_bytes = self.total_bytes.saturating_add(size);
        if !self.blocks.is_empty() && total_bytes > self.max_bytes {
            return false;
        }

        self.total_bytes = total_bytes;
        self.blocks.push(block);
        self.blocks.len() < self.count
    }
}
