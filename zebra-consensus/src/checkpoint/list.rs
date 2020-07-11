//! Checkpoint lists for checkpoint-based block verification
//!
//! Each checkpoint consists of a coinbase height and block header hash.
//!
//! Checkpoints can be used to verify their ancestors, by chaining backwards
//! to another checkpoint, via each block's parent block hash.

use std::{
    collections::{BTreeMap, HashSet},
    error,
    ops::RangeBounds,
};

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
#[derive(Debug)]
pub struct CheckpointList(BTreeMap<BlockHeight, BlockHeaderHash>);

impl CheckpointList {
    /// Create a new checkpoint list from `checkpoint_list`.
    ///
    /// Checkpoint heights and checkpoint hashes must be unique.
    ///
    /// There must be a checkpoint for the genesis block at BlockHeight 0.
    /// (All other checkpoints are optional.)
    pub fn new(
        checkpoint_list: impl IntoIterator<Item = (BlockHeight, BlockHeaderHash)>,
    ) -> Result<Self, Error> {
        // BTreeMap silently ignores duplicates, so we count the checkpoints
        // before adding them to the map
        let original_checkpoints: Vec<(BlockHeight, BlockHeaderHash)> =
            checkpoint_list.into_iter().collect();
        let original_len = original_checkpoints.len();

        let checkpoints: BTreeMap<BlockHeight, BlockHeaderHash> =
            original_checkpoints.into_iter().collect();

        // An empty checkpoint list can't actually verify any blocks.
        match checkpoints.keys().next() {
            Some(BlockHeight(0)) => {}
            None => Err("there must be at least one checkpoint, for the genesis block")?,
            _ => Err("checkpoints must start at the genesis block height 0")?,
        };

        // This check rejects duplicate heights, whether they have the same or
        // different hashes
        if checkpoints.len() != original_len {
            Err("checkpoint heights must be unique")?;
        }

        let block_hashes: HashSet<&BlockHeaderHash> = checkpoints.values().collect();
        if block_hashes.len() != original_len {
            Err("checkpoint hashes must be unique")?;
        }

        // Make sure all the hashes are valid. In Bitcoin, [0; 32] is the null
        // hash. It is also used as the parent hash of genesis blocks.
        if block_hashes.contains(&BlockHeaderHash([0; 32])) {
            Err("the null hash is not a valid checkpoint hash")?;
        }

        let checkpoints = CheckpointList(checkpoints);
        if checkpoints.max_height() > BlockHeight::MAX {
            Err("checkpoint heights must be less than or equal to the maxiumum block height")?;
        }

        Ok(checkpoints)
    }

    /// Is there a checkpoint at `height`?
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
    ///
    /// The maximum height is constant for each checkpoint list.
    pub fn max_height(&self) -> BlockHeight {
        self.0
            .keys()
            .cloned()
            .next_back()
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

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::Arc;

    use zebra_chain::{block::Block, serialization::ZcashDeserialize};

    /// Make a checkpoint list containing only the genesis block
    #[test]
    fn checkpoint_list_genesis() -> Result<(), Error> {
        // Parse the genesis block
        let mut checkpoint_data = Vec::new();
        let block =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..])?;
        let hash: BlockHeaderHash = block.as_ref().into();
        checkpoint_data.push((
            block.coinbase_height().expect("test block has height"),
            hash,
        ));

        // Make a checkpoint list containing the genesis block
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_data.iter().cloned().collect();
        let _ = CheckpointList::new(checkpoint_list)?;

        Ok(())
    }

    /// Make a checkpoint list containing multiple blocks
    #[test]
    fn checkpoint_list_multiple() -> Result<(), Error> {
        // Parse all the blocks
        let mut checkpoint_data = Vec::new();
        for b in &[
            &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_415000_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_434873_BYTES[..],
        ] {
            let block = Arc::<Block>::zcash_deserialize(*b)?;
            let hash: BlockHeaderHash = block.as_ref().into();
            checkpoint_data.push((
                block.coinbase_height().expect("test block has height"),
                hash,
            ));
        }

        // Make a checkpoint list containing all the blocks
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_data.iter().cloned().collect();
        let _ = CheckpointList::new(checkpoint_list)?;

        Ok(())
    }

    /// Make sure that an empty checkpoint list fails
    #[test]
    fn checkpoint_list_empty_fail() -> Result<(), Error> {
        let _ = CheckpointList::new(Vec::new()).expect_err("empty checkpoint lists should fail");

        Ok(())
    }

    /// Make sure a checkpoint list that doesn't contain the genesis block fails
    #[test]
    fn checkpoint_list_no_genesis_fail() -> Result<(), Error> {
        // Parse a non-genesis block
        let mut checkpoint_data = Vec::new();
        let block =
            Arc::<Block>::zcash_deserialize(&zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..])?;
        let hash: BlockHeaderHash = block.as_ref().into();
        checkpoint_data.push((
            block.coinbase_height().expect("test block has height"),
            hash,
        ));

        // Make a checkpoint list containing the non-genesis block
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_data.iter().cloned().collect();
        let _ = CheckpointList::new(checkpoint_list)
            .expect_err("a checkpoint list with no genesis block should fail");

        Ok(())
    }

    /// Make sure a checkpoint list that contains a null hash fails
    #[test]
    fn checkpoint_list_null_hash_fail() -> Result<(), Error> {
        let checkpoint_data = vec![(BlockHeight(0), BlockHeaderHash([0; 32]))];

        // Make a checkpoint list containing the non-genesis block
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_data.iter().cloned().collect();
        let _ = CheckpointList::new(checkpoint_list)
            .expect_err("a checkpoint list with a null block hash should fail");

        Ok(())
    }

    /// Make sure a checkpoint list that contains an invalid block height fails
    #[test]
    fn checkpoint_list_bad_height_fail() -> Result<(), Error> {
        let checkpoint_data = vec![(
            BlockHeight(BlockHeight::MAX.0 + 1),
            BlockHeaderHash([1; 32]),
        )];

        // Make a checkpoint list containing the non-genesis block
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_data.iter().cloned().collect();
        let _ = CheckpointList::new(checkpoint_list).expect_err(
            "a checkpoint list with an invalid block height (BlockHeight::MAX + 1) should fail",
        );

        let checkpoint_data = vec![(BlockHeight(u32::MAX), BlockHeaderHash([1; 32]))];

        // Make a checkpoint list containing the non-genesis block
        let checkpoint_list: BTreeMap<BlockHeight, BlockHeaderHash> =
            checkpoint_data.iter().cloned().collect();
        let _ = CheckpointList::new(checkpoint_list)
            .expect_err("a checkpoint list with an invalid block height (u32::MAX) should fail");

        Ok(())
    }

    /// Make sure that a checkpoint list containing duplicate blocks fails
    #[test]
    fn checkpoint_list_duplicate_blocks_fail() -> Result<(), Error> {
        // Parse some blocks twice
        let mut checkpoint_data = Vec::new();
        for b in &[
            &zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
            &zebra_test::vectors::BLOCK_MAINNET_1_BYTES[..],
        ] {
            let block = Arc::<Block>::zcash_deserialize(*b)?;
            let hash: BlockHeaderHash = block.as_ref().into();
            checkpoint_data.push((
                block.coinbase_height().expect("test block has height"),
                hash,
            ));
        }

        // Make a checkpoint list containing some duplicate blocks
        let _ = CheckpointList::new(checkpoint_data)
            .expect_err("checkpoint lists with duplicate blocks should fail");

        Ok(())
    }

    /// Make sure that a checkpoint list containing duplicate heights
    /// (with different hashes) fails
    #[test]
    fn checkpoint_list_duplicate_heights_fail() -> Result<(), Error> {
        // Parse the genesis block
        let mut checkpoint_data = Vec::new();
        for b in &[&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]] {
            let block = Arc::<Block>::zcash_deserialize(*b)?;
            let hash: BlockHeaderHash = block.as_ref().into();
            checkpoint_data.push((
                block.coinbase_height().expect("test block has height"),
                hash,
            ));
        }

        // Then add some fake entries with duplicate heights
        checkpoint_data.push((BlockHeight(1), BlockHeaderHash([0xaa; 32])));
        checkpoint_data.push((BlockHeight(1), BlockHeaderHash([0xbb; 32])));

        // Make a checkpoint list containing some duplicate blocks
        let _ = CheckpointList::new(checkpoint_data)
            .expect_err("checkpoint lists with duplicate heights should fail");

        Ok(())
    }

    /// Make sure that a checkpoint list containing duplicate hashes
    /// (at different heights) fails
    #[test]
    fn checkpoint_list_duplicate_hashes_fail() -> Result<(), Error> {
        // Parse the genesis block
        let mut checkpoint_data = Vec::new();
        for b in &[&zebra_test::vectors::BLOCK_MAINNET_GENESIS_BYTES[..]] {
            let block = Arc::<Block>::zcash_deserialize(*b)?;
            let hash: BlockHeaderHash = block.as_ref().into();
            checkpoint_data.push((
                block.coinbase_height().expect("test block has height"),
                hash,
            ));
        }

        // Then add some fake entries with duplicate hashes
        checkpoint_data.push((BlockHeight(1), BlockHeaderHash([0xcc; 32])));
        checkpoint_data.push((BlockHeight(2), BlockHeaderHash([0xcc; 32])));

        // Make a checkpoint list containing some duplicate blocks
        let _ = CheckpointList::new(checkpoint_data)
            .expect_err("checkpoint lists with duplicate hashes should fail");

        Ok(())
    }
}
