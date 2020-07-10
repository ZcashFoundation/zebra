//! Checkpoint lists for checkpoint-based block verification
//!
//! Each checkpoint consists of a coinbase height and block header hash.
//!
//! Checkpoints can be used to verify their ancestors, by chaining backwards
//! to another checkpoint, via each block's parent block hash.

use std::{collections::BTreeMap, error, ops::RangeBounds};

use zebra_chain::block::BlockHeaderHash;
use zebra_chain::types::BlockHeight;

/// The inner error type for CheckpointVerifier.
// TODO(jlusby): Error = Report ?
type Error = Box<dyn error::Error + Send + Sync + 'static>;

/// Checkpoint lists are implemented using a BTreeMap.
///
/// Checkpoints should be chosen to avoid forks or chain reorganizations,
/// which only happen in the last few hundred blocks in the chain.
/// (zcashd allows chain reorganizations up to 99 blocks, and prunes
/// orphaned side-chains after 288 blocks.)
///
/// There must be a checkpoint for the genesis block at BlockHeight 0.
/// (All other checkpoints are optional.)
#[derive(Debug)]
pub struct CheckpointList(BTreeMap<BlockHeight, BlockHeaderHash>);

impl CheckpointList {
    /// Create a new checkpoint list from `checkpoint_list`.
    pub fn new(
        checkpoint_list: impl IntoIterator<Item = (BlockHeight, BlockHeaderHash)>,
    ) -> Result<Self, Error> {
        let checkpoints: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_list.into_iter().collect();

        // An empty checkpoint list can't actually verify any blocks.
        match checkpoints.keys().next() {
            Some(BlockHeight(0)) => {}
            None => Err("there must be at least one checkpoint, for the genesis block")?,
            _ => Err("checkpoints must start at the genesis block height 0")?,
        };

        Ok(CheckpointList(checkpoints))
    }

    /// Return true if there is a checkpoint at `height`.
    ///
    /// See `BTreeMap::contains_key()` for details.
    pub fn contains(&self, height: BlockHeight) -> bool {
        self.0.contains_key(&height)
    }

    /// Returns the hash corresponding to the checkpoint at `height`,
    /// or None if there is no checkpoint at that height.
    ///
    /// See `BTreeMap::get()` for details.
    pub fn hash(&self, height: BlockHeight) -> Option<BlockHeaderHash> {
        self.0.get(&height).cloned()
    }

    /// Return the block height of the highest checkpoint in the checkpoint list.
    ///
    /// If there is only a single checkpoint, then the maximum height will be
    /// zero. (The genesis block.)
    pub fn max_height(&self) -> BlockHeight {
        self.max_height_in_range(..)
            .expect("checkpoint lists must have at least one checkpoint")
    }

    /// Return the block height of the highest checkpoint in a sub-range.
    pub fn max_height_in_range<R>(&self, range: R) -> Option<BlockHeight>
    where
        R: RangeBounds<BlockHeight>,
    {
        self.0.range(range).map(|(height, _)| *height).next_back()
    }
}
